# sherpa-onnx v1.13.5 C++ ABI declarations

`voice_activity_detector_abi.h` contains only the ABI-relevant declarations
needed by Scribe's narrow Silero posterior bridge. They are derived from these
exact upstream Apache-2.0 files at tag `v1.13.5`:

- <https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.5/sherpa-onnx/csrc/voice-activity-detector.h>
- <https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.5/sherpa-onnx/csrc/vad-model-config.h>
- <https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.5/sherpa-onnx/csrc/silero-vad-model-config.h>
- <https://github.com/k2-fsa/sherpa-onnx/blob/v1.13.5/sherpa-onnx/csrc/ten-vad-model-config.h>

The upstream include graph was reduced to a forward declaration of
`ParseOptions`; ABI-relevant member order, types, default values, constructors,
and `VoiceActivityDetector` method signatures are unchanged. The linked
implementation comes only from the separately size/SHA-reviewed sherpa-onnx
v1.13.5 static archive used by `vendor/sherpa-onnx-sys`.

The combined declaration header preserves the pertinent upstream Xiaomi
Corporation copyright notices from 2023 and 2025 and is explicitly marked as
modified by Scribe for the include-graph reduction.

sherpa-onnx is licensed under Apache License 2.0. The exact license text is
already vendored at `vendor/sherpa-onnx-sys/LICENSE` and covers these
declarations.
