# Third-party components

Rustle itself is MIT licensed. It downloads or bundles the following, each
under its own licence.

| Component | Licence | Used for |
|---|---|---|
| [NVIDIA Parakeet TDT 0.6B v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) | CC-BY-4.0 | Speech recognition (downloaded on first run) |
| [istupakov/parakeet-tdt-0.6b-v3-onnx](https://huggingface.co/istupakov/parakeet-tdt-0.6b-v3-onnx) | CC-BY-4.0 (model), export scripts MIT | The ONNX export Rustle loads |
| [onnx-asr](https://github.com/istupakov/onnx-asr) | MIT | The `nemo128.onnx` mel preprocessor and the reference TDT decoder |
| [ONNX Runtime](https://github.com/microsoft/onnxruntime) via [ort](https://github.com/pykeio/ort) | MIT | Inference |
| [Dawn](https://dawn.googlesource.com/dawn) (`libwebgpu_dawn`, `webgpu_dawn.dll`) | BSD-3-Clause | WebGPU for GPU recognition, shipped with the app |
| [DirectX Shader Compiler](https://github.com/microsoft/DirectXShaderCompiler) (`dxcompiler.dll`, `dxil.dll`, Windows) | see its repository | Dawn's shader compiler on Direct3D 12, shipped with the Windows app |
| [Handy](https://github.com/cjpais/Handy) | MIT | Reference for the overlay, hotkey and clipboard-restore designs |
| [Tauri](https://tauri.app) and its plugins | MIT / Apache-2.0 | App shell, windows, tray, updater, installers |
| Rust crates listed in `Cargo.lock` | see each crate | |
| npm packages listed in `pnpm-lock.yaml` | see each package | |

Cleanup models (Qwen3 through Ollama, or a cloud provider) are chosen and
installed by the user and are not part of this distribution.
