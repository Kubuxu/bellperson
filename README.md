# bellperson [![Crates.io](https://img.shields.io/crates/v/bellperson.svg)](https://crates.io/crates/bellperson)

> This is a fork of the great [bellman](https://github.com/zkcrypto/bellman) library.

`bellman` is a crate for building zk-SNARK circuits. It provides circuit traits
and primitive structures, as well as basic gadget implementations such as
booleans and number abstractions.

## Backend

There are currently two backends available for the implementation of Bls12 381:
- [`blstrs`](https://github.com/filecoin-project/blstrs) - optimized with hand tuned assembly, using [blst](https://github.com/supranational/blst)
- [`paired`](https://github.com/filecoin-project/paired) - pure Rust implementation

They can be selected at compile time with the mutually exclusive features `blst` and `pairing`. Specifying one of them is enough for a working library, no additional features need to be set.

The default for now is `blst`, as the secure and audited choice.  Note that `pairing` is deprecated and may be removed in the future.

## GPU

This fork contains GPU parallel acceleration to the FFT and Multiexponentation algorithms in the groth16 prover codebase under the compilation feature `gpu`, it can be used in combination with `blst` or `pairing`.

### Requirements
- NVIDIA or AMD GPU Graphics Driver
- OpenCL

( For AMD devices we recommend [ROCm](https://rocm-documentation.readthedocs.io/en/latest/Installation_Guide/Installation-Guide.html) )

### Environment variables

The gpu extension contains some env vars that may be set externally to this library.

- `BELLMAN_NO_GPU`

    Will disable the GPU feature from the library and force usage of the CPU.

    ```rust
    // Example
    env::set_var("BELLMAN_NO_GPU", "1");
    ```

- `BELLMAN_VERIFIER`

    Chooses the device in which the batched verifier is going to run. Can be `cpu`, `gpu` or `auto`.

    ```rust
    Example
    env::set_var("BELLMAN_VERIFIER", "gpu");
    ```

- `BELLMAN_CUSTOM_GPU`

    Will allow for adding a GPU not in the tested list. This requires researching the name of the GPU device and the number of cores in the format `["name:cores"]`.

    ```rust
    // Example
    env::set_var("BELLMAN_CUSTOM_GPU", "GeForce RTX 2080 Ti:4352, GeForce GTX 1060:1280");
    ```

- `BELLMAN_CPU_UTILIZATION`

    Can be set in the interval [0,1] to designate a proportion of the multiexponenation calculation to be moved to cpu in parallel to the GPU to keep all hardware occupied.

    ```rust
    // Example
    env::set_var("BELLMAN_CPU_UTILIZATION", "0.5");
    ```

- `RAYON_NUM_THREADS`
   
   Restricts the number of threads used in the library to roughly twice that number (best effort). In the past this was done using `BELLMAN_NUM_CPUS` which is now deprecated. The default is set to the number of logical cores reported on the machine.
   
   ```rust
    // Example
    env::set_var("RAYON_NUM_THREADS", "6");
   ```

#### Supported / Tested Cards

Depending on the size of the proof being passed to the gpu for work, certain cards will not be able to allocate enough memory to either the FFT or Multiexp kernel. Below are a list of devices that work for small sets. In the future we will add the cuttoff point at which a given card will not be able to allocate enough memory to utilize the GPU.

| Device Name            | Cores | Comments       |
|------------------------|-------|----------------|
| Quadro RTX 6000        | 4608  |                |
| TITAN RTX              | 4608  |                |
| Tesla V100             | 5120  |                |
| Tesla P100             | 3584  |                |
| Tesla T4               | 2560  |                |
| Quadro M5000           | 2048  |                |
| GeForce RTX 3090       |10496  |                |
| GeForce RTX 3080       | 8704  |                |
| GeForce RTX 3070       | 5888  |                |
| GeForce RTX 2080 Ti    | 4352  |                |
| GeForce RTX 2080 SUPER | 3072  |                |
| GeForce RTX 2080       | 2944  |                |
| GeForce RTX 2070 SUPER | 2560  |                |
| GeForce GTX 1080 Ti    | 3584  |                |
| GeForce GTX 1080       | 2560  |                |
| GeForce GTX 2060       | 1920  |                |
| GeForce GTX 1660 Ti    | 1536  |                |
| GeForce GTX 1060       | 1280  |                |
| GeForce GTX 1650 SUPER | 1280  |                |
| GeForce GTX 1650       |  896  |                |
|                        |       |                |
| gfx1010                | 2560  | AMD RX 5700 XT |
| gfx906                 | 7400  | AMD RADEON VII |
|------------------------|-------|----------------|

### Running Tests

To run using the `pairing` backend, you can use:

```bash
RUSTFLAGS="-C target-cpu=native" cargo test --release --all --no-default-features --features pairing
```

To run using both the `gpu` and `blst` backend, you can use:

```bash
RUSTFLAGS="-C target-cpu=native" cargo test --release --all --no-default-features --features gpu,blst
```

To run the multiexp_consistency test you can use:

```bash
RUST_LOG=info cargo test --features gpu -- --exact multiexp::gpu_multiexp_consistency --nocapture
```

### Considerations

Bellperson uses `rust-gpu-tools` as its OpenCL backend, therefore you may see a
directory named `~/.rust-gpu-tools` in your home folder, which contains the
compiled binaries of OpenCL kernels used in this repository.

## License

Licensed under either of

- Apache License, Version 2.0, |[LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
