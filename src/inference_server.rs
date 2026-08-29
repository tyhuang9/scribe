//! worker-only native runtime ASR server, compiled only by the dedicated worker.

use anyhow::{Result, anyhow, bail};
use sherpa_onnx::{
    OfflineCanaryModelConfig, OfflineMoonshineModelConfig, OfflineNemoEncDecCtcModelConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, OnlineRecognizer,
    OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig,
};

use crate::onnx_worker::{
    ValidatedOnnxModel, WorkerRecognizer, WorkerRecognizerFactory, validate_pcm_samples,
};
use crate::runtime_artifact::{OnnxFileRole, OnnxModelFamily};

struct NativeRecognizerFactory;

enum NativeRecognizer {
    Offline { recognizer: OfflineRecognizer },
    Online { recognizer: OnlineRecognizer },
}

pub(crate) fn run() -> i32 {
    crate::onnx_worker::run_inference_worker_with_factory(&NativeRecognizerFactory)
}

impl WorkerRecognizerFactory for NativeRecognizerFactory {
    type Recognizer = NativeRecognizer;

    fn create(&self, model: &ValidatedOnnxModel) -> Result<Self::Recognizer> {
        if model.family == OnnxModelFamily::OnlineTransducer {
            let config = online_recognizer_config(model)?;
            return OnlineRecognizer::create(&config)
                .map(|recognizer| NativeRecognizer::Online { recognizer })
                .ok_or_else(|| anyhow!("sherpa-onnx failed to create CPU online recognizer"));
        }

        let config = offline_recognizer_config(model)?;
        OfflineRecognizer::create(&config)
            .map(|recognizer| NativeRecognizer::Offline { recognizer })
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create CPU offline recognizer"))
    }
}

impl WorkerRecognizer for NativeRecognizer {
    type Stream = OnlineStream;

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        validate_pcm_samples(samples)?;
        match self {
            Self::Offline { recognizer } => {
                let stream = recognizer.create_stream();
                stream.accept_waveform(16_000, samples);
                recognizer.decode(&stream);
                stream
                    .get_result()
                    .map(|result| result.text)
                    .ok_or_else(|| anyhow!("sherpa-onnx offline recognizer returned no result"))
            }
            Self::Online { recognizer } => {
                let stream = recognizer.create_stream();
                stream.accept_waveform(16_000, samples);
                stream.input_finished();
                decode_online_ready(recognizer, &stream);
                Ok(recognizer
                    .get_result(&stream)
                    .map(|result| result.text)
                    .unwrap_or_default())
            }
        }
    }

    fn start_stream(&self) -> Result<Self::Stream> {
        match self {
            Self::Online { recognizer } => Ok(recognizer.create_stream()),
            Self::Offline { .. } => bail!("streaming requires an online ONNX transducer"),
        }
    }

    fn accept_chunk(&self, stream: &mut Self::Stream, samples: &[f32]) -> Result<()> {
        validate_pcm_samples(samples)?;
        let Self::Online { .. } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        stream.accept_waveform(16_000, samples);
        Ok(())
    }

    fn input_finished(&self, stream: &mut Self::Stream) -> Result<()> {
        let Self::Online { .. } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        stream.input_finished();
        Ok(())
    }

    fn drain_ready(&self, stream: &mut Self::Stream) -> Result<()> {
        let Self::Online { recognizer } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        decode_online_ready(recognizer, stream);
        Ok(())
    }

    fn stream_result(&self, stream: &Self::Stream) -> Result<String> {
        let Self::Online { recognizer } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        Ok(recognizer
            .get_result(stream)
            .map(|result| result.text)
            .unwrap_or_default())
    }
}

fn decode_online_ready(recognizer: &OnlineRecognizer, stream: &OnlineStream) {
    while recognizer.is_ready(stream) {
        recognizer.decode(stream);
    }
}

fn online_recognizer_config(model: &ValidatedOnnxModel) -> Result<OnlineRecognizerConfig> {
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.provider = Some("cpu".into());
    config.model_config.num_threads = i32::from(model.num_threads);
    config.model_config.tokens = Some(model.path(OnnxFileRole::Tokens)?);
    config.model_config.transducer = OnlineTransducerModelConfig {
        encoder: Some(model.path(OnnxFileRole::Encoder)?),
        decoder: Some(model.path(OnnxFileRole::Decoder)?),
        joiner: Some(model.path(OnnxFileRole::Joiner)?),
    };
    Ok(config)
}

fn offline_recognizer_config(model: &ValidatedOnnxModel) -> Result<OfflineRecognizerConfig> {
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.provider = Some("cpu".into());
    config.model_config.num_threads = i32::from(model.num_threads);
    config.model_config.tokens = Some(model.path(OnnxFileRole::Tokens)?);
    match model.family {
        OnnxModelFamily::Moonshine => {
            config.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: model
                    .files
                    .contains_key(&OnnxFileRole::Preprocessor)
                    .then(|| model.path(OnnxFileRole::Preprocessor))
                    .transpose()?,
                encoder: Some(model.path(OnnxFileRole::Encoder)?),
                uncached_decoder: model
                    .files
                    .contains_key(&OnnxFileRole::UncachedDecoder)
                    .then(|| model.path(OnnxFileRole::UncachedDecoder))
                    .transpose()?,
                cached_decoder: model
                    .files
                    .contains_key(&OnnxFileRole::CachedDecoder)
                    .then(|| model.path(OnnxFileRole::CachedDecoder))
                    .transpose()?,
                merged_decoder: model
                    .files
                    .contains_key(&OnnxFileRole::MergedDecoder)
                    .then(|| model.path(OnnxFileRole::MergedDecoder))
                    .transpose()?,
            }
        }
        OnnxModelFamily::NemoCtc => {
            config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                model: Some(model.path(OnnxFileRole::Model)?),
            }
        }
        OnnxModelFamily::Canary => {
            config.model_config.canary = OfflineCanaryModelConfig {
                encoder: Some(model.path(OnnxFileRole::Encoder)?),
                decoder: Some(model.path(OnnxFileRole::Decoder)?),
                src_lang: Some("en".into()),
                tgt_lang: Some("en".into()),
                use_pnc: true,
            }
        }
        OnnxModelFamily::OfflineTransducer => {
            config.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(model.path(OnnxFileRole::Encoder)?),
                decoder: Some(model.path(OnnxFileRole::Decoder)?),
                joiner: Some(model.path(OnnxFileRole::Joiner)?),
            };
            config.model_config.model_type = Some("nemo_transducer".into());
        }
        OnnxModelFamily::OnlineTransducer => {
            bail!("online transducers require the online recognizer")
        }
    }
    Ok(config)
}
