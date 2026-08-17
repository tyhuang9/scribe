// Exact ABI-relevant declarations from sherpa-onnx v1.13.5.
// See PROVENANCE.md. The upstream include graph is reduced to the declarations
// needed by Scribe's bridge; member order and method signatures are unchanged.

#ifndef SCRIBE_SHERPA_ONNX_V1_13_5_VOICE_ACTIVITY_DETECTOR_ABI_H_
#define SCRIBE_SHERPA_ONNX_V1_13_5_VOICE_ACTIVITY_DETECTOR_ABI_H_

#include <cstdint>
#include <memory>
#include <string>
#include <vector>

namespace sherpa_onnx {

class ParseOptions;

struct SileroVadModelConfig {
  std::string model;
  float threshold = 0.5;
  float min_silence_duration = 0.5;
  float min_speech_duration = 0.25;
  int32_t window_size = 512;
  float max_speech_duration = 20;
  float neg_threshold = -1;

  SileroVadModelConfig() = default;
  void Register(ParseOptions *po);
  bool Validate() const;
  std::string ToString() const;
};

struct TenVadModelConfig {
  std::string model;
  float threshold = 0.5;
  float min_silence_duration = 0.5;
  float min_speech_duration = 0.25;
  int32_t window_size = 256;
  float max_speech_duration = 20;

  TenVadModelConfig() = default;
  void Register(ParseOptions *po);
  bool Validate() const;
  std::string ToString() const;
};

struct VadModelConfig {
  SileroVadModelConfig silero_vad;
  TenVadModelConfig ten_vad;
  int32_t sample_rate = 16000;
  int32_t num_threads = 1;
  std::string provider = "cpu";
  bool debug = false;

  VadModelConfig() = default;
  VadModelConfig(const SileroVadModelConfig &silero_vad,
                 const TenVadModelConfig &ten_vad, int32_t sample_rate,
                 int32_t num_threads, const std::string &provider, bool debug)
      : silero_vad(silero_vad),
        ten_vad(ten_vad),
        sample_rate(sample_rate),
        num_threads(num_threads),
        provider(provider),
        debug(debug) {}
  void Register(ParseOptions *po);
  bool Validate() const;
  std::string ToString() const;
};

struct SpeechSegment {
  int32_t start;
  std::vector<float> samples;
};

class VoiceActivityDetector {
 public:
  explicit VoiceActivityDetector(const VadModelConfig &config,
                                 float buffer_size_in_seconds = 60);
  template <typename Manager>
  VoiceActivityDetector(Manager *mgr, const VadModelConfig &config,
                        float buffer_size_in_seconds = 60);
  ~VoiceActivityDetector();

  void AcceptWaveform(const float *samples, int32_t n);
  float Compute(const float *samples, int32_t n);
  bool Empty() const;
  void Pop();
  void Clear();
  const SpeechSegment &Front() const;
  bool IsSpeechDetected() const;
  SpeechSegment CurrentSpeechSegment() const;
  void Reset() const;
  void Flush() const;
  const VadModelConfig &GetConfig() const;

 private:
  class Impl;
  std::unique_ptr<Impl> impl_;
};

}  // namespace sherpa_onnx

#endif  // SCRIBE_SHERPA_ONNX_V1_13_5_VOICE_ACTIVITY_DETECTOR_ABI_H_
