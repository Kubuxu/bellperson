use std::sync::Arc;
use std::time::Instant;

use crate::bls::Engine;
use blstrs::ScalarEngine;
use ff::{Field, PrimeField};
use groupy::{CurveAffine, CurveProjective};
use rand_core::RngCore;
use rayon::prelude::*;

use super::{ParameterSource, Proof};
use crate::domain::{EvaluationDomain, Scalar};
use crate::gpu::{LockedFFTKernel, LockedMultiexpKernel};
use crate::multicore::{Worker, RAYON_THREAD_POOL, THREAD_POOL};
use crate::multiexp::{multiexp, DensityTracker, FullDensity};
use crate::{
    Circuit, ConstraintSystem, Index, LinearCombination, SynthesisError, Variable, BELLMAN_VERSION,
};
#[cfg(feature = "gpu")]
use log::trace;
use log::{debug, info};

#[cfg(feature = "gpu")]
use crate::gpu::PriorityLock;

struct ProvingAssignment<E: Engine> {
    // Density of queries
    a_aux_density: DensityTracker,
    b_input_density: DensityTracker,
    b_aux_density: DensityTracker,

    // Evaluations of A, B, C polynomials
    a: Vec<Scalar<E>>,
    b: Vec<Scalar<E>>,
    c: Vec<Scalar<E>>,

    // Assignments of variables
    input_assignment: Vec<E::Fr>,
    aux_assignment: Vec<E::Fr>,
}
use std::fmt;

impl<E: Engine> fmt::Debug for ProvingAssignment<E> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("ProvingAssignment")
            .field("a_aux_density", &self.a_aux_density)
            .field("b_input_density", &self.b_input_density)
            .field("b_aux_density", &self.b_aux_density)
            .field(
                "a",
                &self
                    .a
                    .iter()
                    .map(|v| format!("Fr({:?})", v.0))
                    .collect::<Vec<_>>(),
            )
            .field(
                "b",
                &self
                    .b
                    .iter()
                    .map(|v| format!("Fr({:?})", v.0))
                    .collect::<Vec<_>>(),
            )
            .field(
                "c",
                &self
                    .c
                    .iter()
                    .map(|v| format!("Fr({:?})", v.0))
                    .collect::<Vec<_>>(),
            )
            .field("input_assignment", &self.input_assignment)
            .field("aux_assignment", &self.aux_assignment)
            .finish()
    }
}

impl<E: Engine> PartialEq for ProvingAssignment<E> {
    fn eq(&self, other: &ProvingAssignment<E>) -> bool {
        self.a_aux_density == other.a_aux_density
            && self.b_input_density == other.b_input_density
            && self.b_aux_density == other.b_aux_density
            && self.a == other.a
            && self.b == other.b
            && self.c == other.c
            && self.input_assignment == other.input_assignment
            && self.aux_assignment == other.aux_assignment
    }
}

impl<E: Engine> ConstraintSystem<E> for ProvingAssignment<E> {
    type Root = Self;

    fn new() -> Self {
        Self {
            a_aux_density: DensityTracker::new(),
            b_input_density: DensityTracker::new(),
            b_aux_density: DensityTracker::new(),
            a: vec![],
            b: vec![],
            c: vec![],
            input_assignment: vec![],
            aux_assignment: vec![],
        }
    }

    fn alloc<F, A, AR>(&mut self, _: A, f: F) -> Result<Variable, SynthesisError>
    where
        F: FnOnce() -> Result<E::Fr, SynthesisError>,
        A: FnOnce() -> AR,
        AR: Into<String>,
    {
        self.aux_assignment.push(f()?);
        self.a_aux_density.add_element();
        self.b_aux_density.add_element();

        Ok(Variable(Index::Aux(self.aux_assignment.len() - 1)))
    }

    fn alloc_input<F, A, AR>(&mut self, _: A, f: F) -> Result<Variable, SynthesisError>
    where
        F: FnOnce() -> Result<E::Fr, SynthesisError>,
        A: FnOnce() -> AR,
        AR: Into<String>,
    {
        self.input_assignment.push(f()?);
        self.b_input_density.add_element();

        Ok(Variable(Index::Input(self.input_assignment.len() - 1)))
    }

    fn enforce<A, AR, LA, LB, LC>(&mut self, _: A, a: LA, b: LB, c: LC)
    where
        A: FnOnce() -> AR,
        AR: Into<String>,
        LA: FnOnce(LinearCombination<E>) -> LinearCombination<E> + Sync + Send,
        LB: FnOnce(LinearCombination<E>) -> LinearCombination<E> + Sync + Send,
        LC: FnOnce(LinearCombination<E>) -> LinearCombination<E> + Sync + Send,
    {
        let input_assignment = &self.input_assignment;
        let aux_assignment = &self.aux_assignment;
        let a_aux_density = &mut self.a_aux_density;
        let b_input_density = &mut self.b_input_density;
        let b_aux_density = &mut self.b_aux_density;

        let a = a(LinearCombination::zero());
        let a_res = a.eval(
            // Inputs have full density in the A query
            // because there are constraints of the
            // form x * 0 = 0 for each input.
            None,
            Some(a_aux_density),
            input_assignment,
            aux_assignment,
        );
        let b = b(LinearCombination::zero());
        let b_res = b.eval(
            Some(b_input_density),
            Some(b_aux_density),
            input_assignment,
            aux_assignment,
        );
        let c = c(LinearCombination::zero());
        let c_res = c.eval(
            // There is no C polynomial query,
            // though there is an (beta)A + (alpha)B + C
            // query for all aux variables.
            // However, that query has full density.
            None,
            None,
            input_assignment,
            aux_assignment,
        );

        self.a.push(Scalar(a_res));
        self.b.push(Scalar(b_res));
        self.c.push(Scalar(c_res));
    }

    fn push_namespace<NR, N>(&mut self, _: N)
    where
        NR: Into<String>,
        N: FnOnce() -> NR,
    {
        // Do nothing; we don't care about namespaces in this context.
    }

    fn pop_namespace(&mut self) {
        // Do nothing; we don't care about namespaces in this context.
    }

    fn get_root(&mut self) -> &mut Self::Root {
        self
    }

    fn is_extensible() -> bool {
        true
    }

    fn extend(&mut self, other: Self) {
        self.a_aux_density.extend(other.a_aux_density, false);
        self.b_input_density.extend(other.b_input_density, true);
        self.b_aux_density.extend(other.b_aux_density, false);

        self.a.extend(other.a);
        self.b.extend(other.b);
        self.c.extend(other.c);

        self.input_assignment
            // Skip first input, which must have been a temporarily allocated one variable.
            .extend(&other.input_assignment[1..]);
        self.aux_assignment.extend(other.aux_assignment);
    }
}

pub fn create_random_proof_batch_priority<E, C, R, P: ParameterSource<E>>(
    circuits: Vec<C>,
    params: P,
    rng: &mut R,
    priority: bool,
) -> Result<Vec<Proof<E>>, SynthesisError>
where
    E: Engine,
    C: Circuit<E> + Send,
    R: RngCore,
{
    let r_s = (0..circuits.len()).map(|_| E::Fr::random(rng)).collect();
    let s_s = (0..circuits.len()).map(|_| E::Fr::random(rng)).collect();

    create_proof_batch_priority::<E, C, P>(circuits, params, r_s, s_s, priority)
}

pub fn create_proof_batch_priority<E, C, P: ParameterSource<E>>(
    circuits: Vec<C>,
    params: P,
    r_s: Vec<E::Fr>,
    s_s: Vec<E::Fr>,
    priority: bool,
) -> Result<Vec<Proof<E>>, SynthesisError>
where
    E: Engine,
    C: Circuit<E> + Send,
{
    info!("Bellperson {} is being used!", BELLMAN_VERSION);

    // Preparing things for the proofs is done a lot in parallel with the help of Rayon. Make
    // sure that those things run on the correct thread pool.
    let mut provers = RAYON_THREAD_POOL.install(|| create_proof_batch_priority_inner(circuits))?;

    // Start fft/multiexp prover timer
    let start = Instant::now();

    // The rest of the proving also has parallelism, but not on the outer loops, but within e.g. the
    // multiexp calculations. This is what the `Worker` is used for. It is important that calling
    // `wait()` on the worker happens *outside* the thread pool, else deadlocks can happen.

    let worker = Worker::new();
    let input_len = provers[0].1.len();
    let vk = params.get_vk(input_len)?.clone();
    let n = provers[0].0.a.len();
    let a_aux_density_total = provers[0].0.a_aux_density.get_total_density();
    let b_input_density_total = provers[0].0.b_input_density.get_total_density();
    let b_aux_density_total = provers[0].0.b_aux_density.get_total_density();
    let aux_assignment_len = provers[0].0.aux_assignment.len();
    let num_circuits = provers.len();

    // Make sure all circuits have the same input len.
    for prover in &provers {
        assert_eq!(
            prover.0.a.len(),
            n,
            "only equaly sized circuits are supported"
        );
        debug_assert_eq!(
            a_aux_density_total,
            prover.0.a_aux_density.get_total_density(),
            "only identical circuits are supported"
        );
        debug_assert_eq!(
            b_input_density_total,
            prover.0.b_input_density.get_total_density(),
            "only identical circuits are supported"
        );
        debug_assert_eq!(
            b_aux_density_total,
            prover.0.b_aux_density.get_total_density(),
            "only identical circuits are supported"
        );
    }

    let mut log_d = 0;
    while (1 << log_d) < n {
        log_d += 1;
    }

    #[cfg(feature = "gpu")]
    let prio_lock = if priority {
        trace!("acquiring priority lock");
        Some(PriorityLock::lock())
    } else {
        None
    };

    let mut a_s = Vec::with_capacity(num_circuits);
    let mut params_h = None;
    let worker = &worker;
    let provers_ref = &mut provers;
    let params = &params;

    THREAD_POOL.scoped(|s| {
        let a_s = &mut a_s;
        s.execute(move || {
            let mut fft_kern = Some(LockedFFTKernel::<E>::new(log_d, priority));

            for (i, (prover, _, _)) in provers_ref.iter_mut().enumerate() {
                debug!("fft prover: {}", i);
                a_s.push(execute_fft(worker, prover, &mut fft_kern));
            }
        });

        debug!("params h");
        params_h = Some(params.get_h(n));
        debug!("params h done")
    });

    let a_s = a_s
        .into_iter()
        .collect::<Result<Vec<_>, SynthesisError>>()?;

    let mut multiexp_kern = Some(LockedMultiexpKernel::<E>::new(log_d, priority));
    let params_h = params_h.unwrap()?;

    let mut h_s = Vec::with_capacity(num_circuits);
    let mut params_l = None;
    let mut params_a = None;

    THREAD_POOL.scoped(|s| {
        let params_l = &mut params_l;
        let params_a = &mut params_a;

        s.execute(move || {
            debug!("params l");
            *params_l = Some(params.get_l(aux_assignment_len));
            debug!("params_a");
            *params_a = Some(params.get_a(input_len, a_aux_density_total));
            debug!("params_a done");
        });

        debug!("multiexp h");
        for a in a_s.into_iter() {
            h_s.push(multiexp(
                &worker,
                params_h.clone(),
                FullDensity,
                a,
                &mut multiexp_kern,
            ));
        }
        debug!("multiexp h done");
    });

    let params_l = params_l.unwrap()?;

    let mut l_s = Vec::with_capacity(num_circuits);
    let mut params_b_g1 = None;
    let mut params_b_g2 = None;

    THREAD_POOL.scoped(|s| {
        let params_b_g1 = &mut params_b_g1;
        s.execute(move || {
            debug!("params_b_g1");
            *params_b_g1 = Some(params.get_b_g1(b_input_density_total, b_aux_density_total));
            debug!("params_b_g1 done");
        });

        let params_b_g2 = &mut params_b_g2;
        s.execute(move || {
            debug!("params_b_g2");
            *params_b_g2 = Some(params.get_b_g2(b_input_density_total, b_aux_density_total));
            debug!("params_b_g2 done")
        });

        debug!("multiexp l");
        for (_, _, aux) in provers.iter() {
            l_s.push(multiexp(
                &worker,
                params_l.clone(),
                FullDensity,
                aux.clone(),
                &mut multiexp_kern,
            ));
        }
        debug!("multiexp l done");
    });

    let (a_inputs_source, a_aux_source) = params_a.unwrap()?;
    let (b_g1_inputs_source, b_g1_aux_source) = params_b_g1.unwrap()?;
    let (b_g2_inputs_source, b_g2_aux_source) = params_b_g2.unwrap()?;

    debug!("multiexp a b_g1 b_g2");
    let mut proofs = Vec::with_capacity(num_circuits);
    for (i, (((((prover, input_assignment, aux_assignment), h), l), r), s)) in provers
        .into_iter()
        .zip(h_s.into_iter())
        .zip(l_s.into_iter())
        .zip(r_s.into_iter())
        .zip(s_s.into_iter())
        .enumerate()
    {
        debug!("prover {}", i);

        debug!("multiexp a_inputs");
        let a_inputs = multiexp(
            &worker,
            a_inputs_source.clone(),
            FullDensity,
            input_assignment.clone(),
            &mut multiexp_kern,
        );

        debug!("multiexp a_aux");
        let a_aux = multiexp(
            &worker,
            a_aux_source.clone(),
            Arc::new(prover.a_aux_density),
            aux_assignment.clone(),
            &mut multiexp_kern,
        );

        let b_input_density = Arc::new(prover.b_input_density);
        let b_aux_density = Arc::new(prover.b_aux_density);

        debug!("multiexp, b_g1_inputs");
        let b_g1_inputs = multiexp(
            &worker,
            b_g1_inputs_source.clone(),
            b_input_density.clone(),
            input_assignment.clone(),
            &mut multiexp_kern,
        );
        debug!("multiexp b_g1_aux");
        let b_g1_aux = multiexp(
            &worker,
            b_g1_aux_source.clone(),
            b_aux_density.clone(),
            aux_assignment.clone(),
            &mut multiexp_kern,
        );

        debug!("multiexp b_g2_inputs");
        let b_g2_inputs = multiexp(
            &worker,
            b_g2_inputs_source.clone(),
            b_input_density,
            input_assignment.clone(),
            &mut multiexp_kern,
        );

        debug!("multiexp b_g2_aux");
        let b_g2_aux = multiexp(
            &worker,
            b_g2_aux_source.clone(),
            b_aux_density,
            aux_assignment.clone(),
            &mut multiexp_kern,
        );

        debug!("create proof");
        if vk.delta_g1.is_zero() || vk.delta_g2.is_zero() {
            // If this element is zero, someone is trying to perform a
            // subversion-CRS attack.
            return Err(SynthesisError::UnexpectedIdentity);
        }

        let mut g_a = vk.delta_g1.mul(r);
        g_a.add_assign_mixed(&vk.alpha_g1);
        let mut g_b = vk.delta_g2.mul(s);
        g_b.add_assign_mixed(&vk.beta_g2);
        let mut g_c;
        {
            let mut rs = r;
            rs.mul_assign(&s);

            g_c = vk.delta_g1.mul(rs);
            g_c.add_assign(&vk.alpha_g1.mul(s));
            g_c.add_assign(&vk.beta_g1.mul(r));
        }
        let mut a_answer = a_inputs.wait()?;
        a_answer.add_assign(&a_aux.wait()?);
        g_a.add_assign(&a_answer);
        a_answer.mul_assign(s);
        g_c.add_assign(&a_answer);

        let mut b1_answer = b_g1_inputs.wait()?;
        b1_answer.add_assign(&b_g1_aux.wait()?);
        let mut b2_answer = b_g2_inputs.wait()?;
        b2_answer.add_assign(&b_g2_aux.wait()?);

        g_b.add_assign(&b2_answer);
        b1_answer.mul_assign(r);
        g_c.add_assign(&b1_answer);
        g_c.add_assign(&h.wait()?);
        g_c.add_assign(&l.wait()?);

        proofs.push(Proof {
            a: g_a.into_affine(),
            b: g_b.into_affine(),
            c: g_c.into_affine(),
        });
    }
    #[cfg(feature = "gpu")]
    {
        trace!("dropping priority lock");
        drop(prio_lock);
    }

    let proof_time = start.elapsed();
    info!("prover time: {:?}", proof_time);

    Ok(proofs)
}

fn execute_fft<E>(
    worker: &Worker,
    prover: &mut ProvingAssignment<E>,
    fft_kern: &mut Option<LockedFFTKernel<E>>,
) -> Result<Arc<Vec<<<E as ScalarEngine>::Fr as PrimeField>::Repr>>, SynthesisError>
where
    E: Engine,
{
    let mut a = EvaluationDomain::from_coeffs(std::mem::replace(&mut prover.a, Vec::new()))?;
    let mut b = EvaluationDomain::from_coeffs(std::mem::replace(&mut prover.b, Vec::new()))?;
    let mut c = EvaluationDomain::from_coeffs(std::mem::replace(&mut prover.c, Vec::new()))?;

    debug!("ifft a-b-c");
    EvaluationDomain::ifft3(&mut a, &mut b, &mut c, &worker, fft_kern)?;
    debug!("coset fft a-b-c");
    EvaluationDomain::coset_fft3(&mut a, &mut b, &mut c, &worker, fft_kern)?;

    a.mul_assign(&worker, &b);
    drop(b);
    a.sub_assign(&worker, &c);
    drop(c);

    debug!("divide by z on coset");
    a.divide_by_z_on_coset(&worker);
    debug!("icoset fft a");
    a.icoset_fft(&worker, fft_kern)?;
    debug!("finalize");

    let a = a.into_coeffs();
    let a_len = a.len() - 1;
    let a = a
        .into_par_iter()
        .take(a_len)
        .map(|s| s.0.into_repr())
        .collect::<Vec<_>>();
    Ok(Arc::new(a))
}

#[allow(clippy::type_complexity)]
fn create_proof_batch_priority_inner<E, C>(
    circuits: Vec<C>,
) -> Result<
    std::vec::Vec<(
        ProvingAssignment<E>,
        std::sync::Arc<std::vec::Vec<<E::Fr as PrimeField>::Repr>>,
        std::sync::Arc<std::vec::Vec<<E::Fr as PrimeField>::Repr>>,
    )>,
    SynthesisError,
>
where
    E: Engine,
    C: Circuit<E> + Send,
{
    let start = Instant::now();
    let provers = circuits
        .into_par_iter()
        .enumerate()
        .map(|(i, circuit)| -> Result<_, SynthesisError> {
            debug!("start synthesis: {}", i);
            let start = Instant::now();
            let mut prover = ProvingAssignment::new();

            prover.alloc_input(|| "", || Ok(E::Fr::one()))?;

            circuit.synthesize(&mut prover)?;

            for i in 0..prover.input_assignment.len() {
                prover.enforce(|| "", |lc| lc + Variable(Index::Input(i)), |lc| lc, |lc| lc);
            }
            debug!("done synthesis: {} in {:?}", i, start.elapsed());
            let input_assignment = std::mem::replace(&mut prover.input_assignment, Vec::new());
            let input = Arc::new(
                input_assignment
                    .into_iter()
                    .map(|s| s.into_repr())
                    .collect::<Vec<_>>(),
            );
            let aux_assignment = std::mem::replace(&mut prover.aux_assignment, Vec::new());
            let aux = Arc::new(
                aux_assignment
                    .into_iter()
                    .map(|s| s.into_repr())
                    .collect::<Vec<_>>(),
            );

            Ok((prover, input, aux))
        })
        .collect::<Result<Vec<_>, _>>()?;

    info!("synthesis time total: {:?}", start.elapsed());
    Ok(provers)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bls::{Bls12, Fr};
    use rand::Rng;
    use rand_core::SeedableRng;
    use rand_xorshift::XorShiftRng;

    #[test]
    fn test_proving_assignment_extend() {
        let mut rng = XorShiftRng::from_seed([
            0x59, 0x62, 0xbe, 0x5d, 0x76, 0x3d, 0x31, 0x8d, 0x17, 0xdb, 0x37, 0x32, 0x54, 0x06,
            0xbc, 0xe5,
        ]);

        for k in &[2, 4, 8] {
            for j in &[10, 20, 50] {
                let count: usize = k * j;

                let mut full_assignment = ProvingAssignment::<Bls12>::new();
                full_assignment
                    .alloc_input(|| "one", || Ok(Fr::one()))
                    .unwrap();

                let mut partial_assignments = Vec::with_capacity(count / k);
                for i in 0..count {
                    if i % k == 0 {
                        let mut p = ProvingAssignment::new();
                        p.alloc_input(|| "one", || Ok(Fr::one())).unwrap();
                        partial_assignments.push(p)
                    }

                    let index: usize = i / k;
                    let partial_assignment = &mut partial_assignments[index];

                    if rng.gen() {
                        let el = Fr::random(&mut rng);
                        full_assignment
                            .alloc(|| format!("alloc:{},{}", i, k), || Ok(el))
                            .unwrap();
                        partial_assignment
                            .alloc(|| format!("alloc:{},{}", i, k), || Ok(el))
                            .unwrap();
                    }

                    if rng.gen() {
                        let el = Fr::random(&mut rng);
                        full_assignment
                            .alloc_input(|| format!("alloc_input:{},{}", i, k), || Ok(el))
                            .unwrap();
                        partial_assignment
                            .alloc_input(|| format!("alloc_input:{},{}", i, k), || Ok(el))
                            .unwrap();
                    }

                    // TODO: LinearCombination
                }

                let mut combined = ProvingAssignment::new();
                combined.alloc_input(|| "one", || Ok(Fr::one())).unwrap();

                for assignment in partial_assignments.into_iter() {
                    combined.extend(assignment);
                }
                assert_eq!(combined, full_assignment);
            }
        }
    }
}
