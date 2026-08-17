#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <exception>
#include <memory>
#include <string>

#include "sherpa-onnx-v1.13.5/voice_activity_detector_abi.h"

namespace {

constexpr std::size_t kWindowSamples = 512;

void SetError(char *error, std::size_t error_capacity,
              const char *message) noexcept {
  if (error == nullptr || error_capacity == 0) {
    return;
  }
  const char *safe_message = message == nullptr ? "unknown native error" : message;
  std::strncpy(error, safe_message, error_capacity - 1);
  error[error_capacity - 1] = '\0';
}

struct ScribeSileroVad {
  std::unique_ptr<sherpa_onnx::VoiceActivityDetector> detector;
};

}  // namespace

extern "C" int32_t scribe_silero_vad_create(
    const char *model_path, int32_t num_threads, void **out_handle, char *error,
    std::size_t error_capacity) noexcept {
  if (model_path == nullptr || model_path[0] == '\0' || out_handle == nullptr ||
      num_threads <= 0 || num_threads > 64) {
    SetError(error, error_capacity, "invalid Silero VAD creation arguments");
    return 1;
  }
  *out_handle = nullptr;
  try {
    sherpa_onnx::VadModelConfig config;
    config.silero_vad.model = model_path;
    config.silero_vad.threshold = 0.5f;
    config.silero_vad.min_silence_duration = 0.5f;
    config.silero_vad.min_speech_duration = 0.25f;
    config.silero_vad.window_size = static_cast<int32_t>(kWindowSamples);
    config.silero_vad.max_speech_duration = 20.0f;
    config.sample_rate = 16000;
    config.num_threads = num_threads;
    config.provider = "cpu";
    config.debug = false;
    if (!config.Validate()) {
      SetError(error, error_capacity, "sherpa-onnx rejected the fixed VAD config");
      return 2;
    }
    auto handle = std::make_unique<ScribeSileroVad>();
    handle->detector = std::make_unique<sherpa_onnx::VoiceActivityDetector>(
        config, 1.0f);
    *out_handle = handle.release();
    return 0;
  } catch (const std::exception &exception) {
    SetError(error, error_capacity, exception.what());
    return 3;
  } catch (...) {
    SetError(error, error_capacity, "unknown exception creating Silero VAD");
    return 3;
  }
}

extern "C" int32_t scribe_silero_vad_compute_exact_512(
    void *opaque, const float *samples, std::size_t sample_count,
    float *out_probability, char *error, std::size_t error_capacity) noexcept {
  if (opaque == nullptr || samples == nullptr || out_probability == nullptr ||
      sample_count != kWindowSamples) {
    SetError(error, error_capacity,
             "Silero VAD input must contain exactly 512 samples");
    return 1;
  }
  for (std::size_t i = 0; i != sample_count; ++i) {
    if (!std::isfinite(samples[i]) || samples[i] < -1.0f ||
        samples[i] > 1.0f) {
      SetError(error, error_capacity,
               "Silero VAD samples must be finite and within [-1, 1]");
      return 1;
    }
  }
  try {
    auto *handle = static_cast<ScribeSileroVad *>(opaque);
    const float probability = handle->detector->Compute(
        samples, static_cast<int32_t>(sample_count));
    if (!std::isfinite(probability) || probability < 0.0f ||
        probability > 1.0f) {
      SetError(error, error_capacity,
               "sherpa-onnx returned an invalid speech probability");
      return 4;
    }
    *out_probability = probability;
    return 0;
  } catch (const std::exception &exception) {
    SetError(error, error_capacity, exception.what());
    return 3;
  } catch (...) {
    SetError(error, error_capacity, "unknown exception computing Silero VAD");
    return 3;
  }
}

extern "C" int32_t scribe_silero_vad_reset(
    void *opaque, char *error, std::size_t error_capacity) noexcept {
  if (opaque == nullptr) {
    SetError(error, error_capacity, "Silero VAD handle is null");
    return 1;
  }
  try {
    static_cast<ScribeSileroVad *>(opaque)->detector->Reset();
    return 0;
  } catch (const std::exception &exception) {
    SetError(error, error_capacity, exception.what());
    return 3;
  } catch (...) {
    SetError(error, error_capacity, "unknown exception resetting Silero VAD");
    return 3;
  }
}

extern "C" void scribe_silero_vad_destroy(void *opaque) noexcept {
  try {
    delete static_cast<ScribeSileroVad *>(opaque);
  } catch (...) {
  }
}
