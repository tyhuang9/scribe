use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TextNormalizationOptions {
    pub normalize_numbers: bool,
    pub expand_contractions: bool,
    pub remove_fillers: bool,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BenchmarkMetric {
    Wer,
    Cer,
    Wip,
    Wil,
    Latency,
    Rtf,
    Ram,
    Vram,
}

impl BenchmarkMetric {
    pub fn header(self) -> &'static str {
        match self.direction() {
            MetricDirection::LowerIsBetter => match self {
                Self::Wer => "WER (low)",
                Self::Cer => "CER (low)",
                Self::Wil => "WIL (low)",
                Self::Latency => "Latency (low)",
                Self::Rtf => "RTF (low)",
                Self::Ram => "RAM (low)",
                Self::Vram => "VRAM (low)",
                Self::Wip => "WIP",
            },
            MetricDirection::HigherIsBetter => "WIP (high)",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Wer => {
                "Word error rate: substitutions, deletions, and insertions divided by reference words. Lower is better."
            }
            Self::Cer => {
                "Character error rate: character edit distance divided by reference characters. Lower is better."
            }
            Self::Wip => {
                "Word information preserved: retained reference and prediction word overlap. Higher is better."
            }
            Self::Wil => "Word information lost: one minus WIP. Lower is better.",
            Self::Latency => {
                "End-to-end transcription time including model load and post-processing. Lower is better."
            }
            Self::Rtf => {
                "Real-time factor: elapsed transcription time divided by audio duration. Lower is better."
            }
            Self::Ram => "Peak system memory observed during transcription. Lower is better.",
            Self::Vram => "Peak GPU memory observed during transcription. Lower is better.",
        }
    }

    pub fn direction(self) -> MetricDirection {
        match self {
            Self::Wip => MetricDirection::HigherIsBetter,
            Self::Wer
            | Self::Cer
            | Self::Wil
            | Self::Latency
            | Self::Rtf
            | Self::Ram
            | Self::Vram => MetricDirection::LowerIsBetter,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetricDirection {
    LowerIsBetter,
    HigherIsBetter,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RankingMode {
    Accuracy,
    Speed,
    LowMemory,
    Balanced,
}

impl RankingMode {
    pub const ALL: [Self; 4] = [Self::Accuracy, Self::Speed, Self::LowMemory, Self::Balanced];

    pub fn label(self) -> &'static str {
        match self {
            Self::Accuracy => "Accuracy",
            Self::Speed => "Speed",
            Self::LowMemory => "Low Memory",
            Self::Balanced => "Balanced",
        }
    }

    fn weights(self) -> &'static [(BenchmarkMetric, f64)] {
        match self {
            Self::Accuracy => &[
                (BenchmarkMetric::Wer, 0.60),
                (BenchmarkMetric::Cer, 0.20),
                (BenchmarkMetric::Wip, 0.10),
                (BenchmarkMetric::Wil, 0.10),
            ],
            Self::Speed => &[
                (BenchmarkMetric::Rtf, 0.50),
                (BenchmarkMetric::Latency, 0.30),
                (BenchmarkMetric::Wer, 0.20),
            ],
            Self::LowMemory => &[
                (BenchmarkMetric::Ram, 0.35),
                (BenchmarkMetric::Vram, 0.35),
                (BenchmarkMetric::Rtf, 0.15),
                (BenchmarkMetric::Wer, 0.15),
            ],
            Self::Balanced => &[
                (BenchmarkMetric::Wer, 0.40),
                (BenchmarkMetric::Cer, 0.15),
                (BenchmarkMetric::Wip, 0.10),
                (BenchmarkMetric::Rtf, 0.20),
                (BenchmarkMetric::Latency, 0.10),
                (BenchmarkMetric::Ram, 0.025),
                (BenchmarkMetric::Vram, 0.025),
            ],
        }
    }
}

#[derive(Clone, Debug)]
pub struct BenchmarkModelInput {
    pub model_id: String,
    pub model_name: String,
    pub predicted_transcript: String,
    pub reference_transcript: String,
    pub elapsed_ms: Option<u128>,
    pub audio_duration_ms: Option<u128>,
    pub peak_ram_mb: Option<f64>,
    pub peak_vram_mb: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct RawBenchmarkMetrics {
    pub wer: Option<f64>,
    pub cer: Option<f64>,
    pub wip: Option<f64>,
    pub wil: Option<f64>,
    pub latency_ms: Option<f64>,
    pub rtf: Option<f64>,
    pub ram_mb: Option<f64>,
    pub vram_mb: Option<f64>,
}

impl RawBenchmarkMetrics {
    pub fn value(&self, metric: BenchmarkMetric) -> Option<f64> {
        match metric {
            BenchmarkMetric::Wer => self.wer,
            BenchmarkMetric::Cer => self.cer,
            BenchmarkMetric::Wip => self.wip,
            BenchmarkMetric::Wil => self.wil,
            BenchmarkMetric::Latency => self.latency_ms,
            BenchmarkMetric::Rtf => self.rtf,
            BenchmarkMetric::Ram => self.ram_mb,
            BenchmarkMetric::Vram => self.vram_mb,
        }
    }

    fn set_value(&mut self, metric: BenchmarkMetric, value: Option<f64>) {
        match metric {
            BenchmarkMetric::Wer => self.wer = value,
            BenchmarkMetric::Cer => self.cer = value,
            BenchmarkMetric::Wip => self.wip = value,
            BenchmarkMetric::Wil => self.wil = value,
            BenchmarkMetric::Latency => self.latency_ms = value,
            BenchmarkMetric::Rtf => self.rtf = value,
            BenchmarkMetric::Ram => self.ram_mb = value,
            BenchmarkMetric::Vram => self.vram_mb = value,
        }
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct BenchmarkModelResult {
    pub model_id: String,
    pub model_name: String,
    pub predicted_transcript: String,
    pub reference_transcript: String,
    pub elapsed_ms: Option<u128>,
    pub audio_duration_ms: Option<u128>,
    pub peak_ram_mb: Option<f64>,
    pub peak_vram_mb: Option<f64>,
    pub raw_metrics: RawBenchmarkMetrics,
    pub normalized_scores: HashMap<BenchmarkMetric, f64>,
    pub overall_scores: HashMap<RankingMode, f64>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct BenchmarkSampleResult {
    pub sample_id: String,
    pub model_results: Vec<BenchmarkModelResult>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct AggregatedModelMetrics {
    pub model_name: String,
    pub per_sample_metrics: Vec<RawBenchmarkMetrics>,
    pub average_metrics: RawBenchmarkMetrics,
    pub worst_case_metrics: RawBenchmarkMetrics,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WordAlignment {
    hits: usize,
    substitutions: usize,
    deletions: usize,
    insertions: usize,
}

impl WordAlignment {
    fn edit_count(self) -> usize {
        self.substitutions + self.deletions + self.insertions
    }

    fn with_hit(mut self) -> Self {
        self.hits += 1;
        self
    }

    fn with_substitution(mut self) -> Self {
        self.substitutions += 1;
        self
    }

    fn with_deletion(mut self) -> Self {
        self.deletions += 1;
        self
    }

    fn with_insertion(mut self) -> Self {
        self.insertions += 1;
        self
    }
}

pub fn normalize_text(input: &str, options: TextNormalizationOptions) -> String {
    let _ = options.normalize_numbers;
    let _ = options.expand_contractions;
    let _ = options.remove_fillers;

    let mut normalized = String::with_capacity(input.len());
    for ch in input.trim().to_lowercase().chars() {
        if ch.is_alphanumeric() || ch.is_whitespace() {
            normalized.push(ch);
        } else if ch == '\'' {
            continue;
        } else {
            normalized.push(' ');
        }
    }

    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn calculate_wer(reference: &str, prediction: &str) -> f64 {
    let reference_words = normalized_words(reference);
    let prediction_words = normalized_words(prediction);

    if reference_words.is_empty() {
        return if prediction_words.is_empty() {
            0.0
        } else {
            1.0
        };
    }

    let alignment = align_words(&reference_words, &prediction_words);
    alignment.edit_count() as f64 / reference_words.len() as f64
}

pub fn calculate_cer(reference: &str, prediction: &str) -> f64 {
    let reference = normalize_text(reference, TextNormalizationOptions::default());
    let prediction = normalize_text(prediction, TextNormalizationOptions::default());
    let reference_chars = reference.chars().collect::<Vec<_>>();
    let prediction_chars = prediction.chars().collect::<Vec<_>>();

    if reference_chars.is_empty() {
        return if prediction_chars.is_empty() {
            0.0
        } else {
            1.0
        };
    }

    levenshtein_distance(&reference_chars, &prediction_chars) as f64 / reference_chars.len() as f64
}

pub fn calculate_wip(reference: &str, prediction: &str) -> f64 {
    let reference_words = normalized_words(reference);
    let prediction_words = normalized_words(prediction);

    match (reference_words.is_empty(), prediction_words.is_empty()) {
        (true, true) => 1.0,
        (true, false) | (false, true) => 0.0,
        (false, false) => {
            let alignment = align_words(&reference_words, &prediction_words);
            let hits = alignment.hits as f64;
            (hits / reference_words.len() as f64) * (hits / prediction_words.len() as f64)
        }
    }
}

pub fn calculate_wil(reference: &str, prediction: &str) -> f64 {
    1.0 - calculate_wip(reference, prediction)
}

pub fn calculate_rtf(elapsed_ms: Option<u128>, audio_duration_ms: Option<u128>) -> Option<f64> {
    let elapsed_ms = elapsed_ms?;
    let audio_duration_ms = audio_duration_ms?;
    if audio_duration_ms == 0 {
        return None;
    }
    Some(elapsed_ms as f64 / audio_duration_ms as f64)
}

pub fn score_benchmark_models(inputs: Vec<BenchmarkModelInput>) -> Vec<BenchmarkModelResult> {
    let mut results = inputs
        .into_iter()
        .map(|input| {
            let raw_metrics = raw_metrics_for_input(&input);
            BenchmarkModelResult {
                model_id: input.model_id,
                model_name: input.model_name,
                predicted_transcript: input.predicted_transcript,
                reference_transcript: input.reference_transcript,
                elapsed_ms: input.elapsed_ms,
                audio_duration_ms: input.audio_duration_ms,
                peak_ram_mb: input.peak_ram_mb,
                peak_vram_mb: input.peak_vram_mb,
                raw_metrics,
                normalized_scores: HashMap::new(),
                overall_scores: HashMap::new(),
            }
        })
        .collect::<Vec<_>>();

    apply_normalized_scores(&mut results);
    apply_overall_scores(&mut results);
    results
}

pub fn normalize_metric_scores(
    values: &[Option<f64>],
    direction: MetricDirection,
) -> Vec<Option<f64>> {
    let available = values.iter().flatten().copied().collect::<Vec<_>>();
    if available.is_empty() {
        return vec![None; values.len()];
    }

    let min = available.iter().copied().fold(f64::INFINITY, f64::min);
    let max = available.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if (max - min).abs() <= f64::EPSILON {
        return values
            .iter()
            .map(|value| value.map(|_| 0.5))
            .collect::<Vec<_>>();
    }

    values
        .iter()
        .map(|value| {
            value.map(|value| match direction {
                MetricDirection::LowerIsBetter => (max - value) / (max - min),
                MetricDirection::HigherIsBetter => (value - min) / (max - min),
            })
        })
        .collect()
}

pub fn calculate_overall_score(
    normalized_scores: &HashMap<BenchmarkMetric, f64>,
    mode: RankingMode,
) -> Option<f64> {
    let mut weighted_total = 0.0;
    let mut available_weight = 0.0;

    for (metric, weight) in mode.weights() {
        if let Some(score) = normalized_scores.get(metric) {
            weighted_total += score * weight;
            available_weight += weight;
        }
    }

    if available_weight > 0.0 {
        Some(weighted_total / available_weight)
    } else {
        None
    }
}

#[allow(dead_code)]
pub fn aggregate_metrics_by_model(
    samples: &[BenchmarkSampleResult],
) -> Vec<AggregatedModelMetrics> {
    let mut grouped: HashMap<String, Vec<RawBenchmarkMetrics>> = HashMap::new();
    for sample in samples {
        for result in &sample.model_results {
            grouped
                .entry(result.model_name.clone())
                .or_default()
                .push(result.raw_metrics.clone());
        }
    }

    let mut aggregated = grouped
        .into_iter()
        .map(|(model_name, per_sample_metrics)| AggregatedModelMetrics {
            model_name,
            average_metrics: aggregate_metrics(&per_sample_metrics, aggregate_average),
            worst_case_metrics: aggregate_metrics(&per_sample_metrics, aggregate_worst_case),
            per_sample_metrics,
        })
        .collect::<Vec<_>>();
    aggregated.sort_by(|a, b| a.model_name.cmp(&b.model_name));
    aggregated
}

pub fn format_metric_value(metric: BenchmarkMetric, value: Option<f64>) -> String {
    let Some(value) = value else {
        return "n/a".to_owned();
    };

    match metric {
        BenchmarkMetric::Wer
        | BenchmarkMetric::Cer
        | BenchmarkMetric::Wip
        | BenchmarkMetric::Wil => format!("{:.1}%", value * 100.0),
        BenchmarkMetric::Latency => format_duration_ms(value),
        BenchmarkMetric::Rtf => format!("{value:.2}x"),
        BenchmarkMetric::Ram | BenchmarkMetric::Vram => format_memory_mb(value),
    }
}

pub fn format_overall_score(value: Option<f64>) -> String {
    value
        .map(|value| format!("{:.0}%", value * 100.0))
        .unwrap_or_else(|| "n/a".to_owned())
}

fn raw_metrics_for_input(input: &BenchmarkModelInput) -> RawBenchmarkMetrics {
    RawBenchmarkMetrics {
        wer: Some(calculate_wer(
            &input.reference_transcript,
            &input.predicted_transcript,
        )),
        cer: Some(calculate_cer(
            &input.reference_transcript,
            &input.predicted_transcript,
        )),
        wip: Some(calculate_wip(
            &input.reference_transcript,
            &input.predicted_transcript,
        )),
        wil: Some(calculate_wil(
            &input.reference_transcript,
            &input.predicted_transcript,
        )),
        latency_ms: input.elapsed_ms.map(|elapsed| elapsed as f64),
        rtf: calculate_rtf(input.elapsed_ms, input.audio_duration_ms),
        ram_mb: input.peak_ram_mb,
        vram_mb: input.peak_vram_mb,
    }
}

fn apply_normalized_scores(results: &mut [BenchmarkModelResult]) {
    for metric in [
        BenchmarkMetric::Wer,
        BenchmarkMetric::Cer,
        BenchmarkMetric::Wip,
        BenchmarkMetric::Wil,
        BenchmarkMetric::Latency,
        BenchmarkMetric::Rtf,
        BenchmarkMetric::Ram,
        BenchmarkMetric::Vram,
    ] {
        let values = results
            .iter()
            .map(|result| result.raw_metrics.value(metric))
            .collect::<Vec<_>>();
        let scores = normalize_metric_scores(&values, metric.direction());
        for (result, score) in results.iter_mut().zip(scores) {
            if let Some(score) = score {
                result.normalized_scores.insert(metric, score);
            }
        }
    }
}

fn apply_overall_scores(results: &mut [BenchmarkModelResult]) {
    for result in results {
        for mode in RankingMode::ALL {
            if let Some(score) = calculate_overall_score(&result.normalized_scores, mode) {
                result.overall_scores.insert(mode, score);
            }
        }
    }
}

fn normalized_words(input: &str) -> Vec<String> {
    normalize_text(input, TextNormalizationOptions::default())
        .split_whitespace()
        .map(ToOwned::to_owned)
        .collect()
}

fn align_words(reference: &[String], prediction: &[String]) -> WordAlignment {
    let rows = reference.len() + 1;
    let cols = prediction.len() + 1;
    let mut table = vec![vec![WordAlignment::default(); cols]; rows];

    for row in 1..rows {
        table[row][0] = table[row - 1][0].with_deletion();
    }
    for col in 1..cols {
        table[0][col] = table[0][col - 1].with_insertion();
    }

    for row in 1..rows {
        for col in 1..cols {
            let diagonal = if reference[row - 1] == prediction[col - 1] {
                table[row - 1][col - 1].with_hit()
            } else {
                table[row - 1][col - 1].with_substitution()
            };
            let deletion = table[row - 1][col].with_deletion();
            let insertion = table[row][col - 1].with_insertion();
            table[row][col] = best_alignment([diagonal, deletion, insertion]);
        }
    }

    table[reference.len()][prediction.len()]
}

fn best_alignment(candidates: [WordAlignment; 3]) -> WordAlignment {
    candidates
        .into_iter()
        .min_by_key(|alignment| {
            (
                alignment.edit_count(),
                usize::MAX - alignment.hits,
                alignment.substitutions,
                alignment.deletions,
                alignment.insertions,
            )
        })
        .unwrap_or_default()
}

fn levenshtein_distance<T: Eq>(left: &[T], right: &[T]) -> usize {
    let rows = left.len() + 1;
    let cols = right.len() + 1;
    let mut table = vec![vec![0; cols]; rows];

    for (row, row_values) in table.iter_mut().enumerate().take(rows) {
        row_values[0] = row;
    }
    for col in 0..cols {
        table[0][col] = col;
    }

    for row in 1..rows {
        for col in 1..cols {
            let substitution_cost = usize::from(left[row - 1] != right[col - 1]);
            table[row][col] = (table[row - 1][col] + 1)
                .min(table[row][col - 1] + 1)
                .min(table[row - 1][col - 1] + substitution_cost);
        }
    }

    table[left.len()][right.len()]
}

fn aggregate_metrics(
    metrics: &[RawBenchmarkMetrics],
    aggregate: fn(BenchmarkMetric, &[f64]) -> Option<f64>,
) -> RawBenchmarkMetrics {
    let mut result = RawBenchmarkMetrics::default();
    for metric in [
        BenchmarkMetric::Wer,
        BenchmarkMetric::Cer,
        BenchmarkMetric::Wip,
        BenchmarkMetric::Wil,
        BenchmarkMetric::Latency,
        BenchmarkMetric::Rtf,
        BenchmarkMetric::Ram,
        BenchmarkMetric::Vram,
    ] {
        let values = metrics
            .iter()
            .filter_map(|raw| raw.value(metric))
            .collect::<Vec<_>>();
        result.set_value(metric, aggregate(metric, &values));
    }
    result
}

fn aggregate_average(_metric: BenchmarkMetric, values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn aggregate_worst_case(metric: BenchmarkMetric, values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    match metric.direction() {
        MetricDirection::LowerIsBetter => values.iter().copied().reduce(f64::max),
        MetricDirection::HigherIsBetter => values.iter().copied().reduce(f64::min),
    }
}

fn format_duration_ms(value: f64) -> String {
    if value >= 1000.0 {
        format!("{:.2}s", value / 1000.0)
    } else {
        format!("{value:.0} ms")
    }
}

fn format_memory_mb(value: f64) -> String {
    if value >= 1024.0 {
        format!("{:.2} GB", value / 1024.0)
    } else {
        format!("{value:.0} MB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_lowercases_trims_collapses_whitespace_and_removes_punctuation() {
        let normalized = normalize_text(
            "  Hello,   WORLD!!\nThis\tcan't be Scribe.  ",
            TextNormalizationOptions::default(),
        );

        assert_eq!(normalized, "hello world this cant be scribe");
    }

    #[test]
    fn wer_counts_substitutions() {
        assert!((calculate_wer("hello world", "hello scribe") - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn wer_counts_insertions() {
        assert!((calculate_wer("hello world", "hello brave world") - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn wer_counts_deletions() {
        assert!((calculate_wer("hello brave world", "hello world") - (1.0 / 3.0)).abs() < 0.0001);
    }

    #[test]
    fn cer_uses_character_edit_distance() {
        assert!((calculate_cer("abc", "adc") - (1.0 / 3.0)).abs() < 0.0001);
    }

    #[test]
    fn wip_and_wil_are_complements() {
        let wip = calculate_wip("one two three", "one three");
        let wil = calculate_wil("one two three", "one three");

        assert!((wip - (2.0 / 3.0 * 1.0)).abs() < 0.0001);
        assert!((wil - (1.0 - wip)).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_reference_and_prediction_edges_are_safe() {
        assert_eq!(calculate_wer("", ""), 0.0);
        assert_eq!(calculate_cer("", ""), 0.0);
        assert_eq!(calculate_wip("", ""), 1.0);
        assert_eq!(calculate_wil("", ""), 0.0);

        assert_eq!(calculate_wer("", "extra"), 1.0);
        assert_eq!(calculate_cer("", "extra"), 1.0);
        assert_eq!(calculate_wip("reference", ""), 0.0);
        assert_eq!(calculate_wil("reference", ""), 1.0);
        assert_eq!(calculate_rtf(Some(1000), Some(0)), None);
    }

    #[test]
    fn normalized_scoring_lower_is_better() {
        let scores = normalize_metric_scores(
            &[Some(10.0), Some(20.0), Some(30.0)],
            MetricDirection::LowerIsBetter,
        );

        assert_eq!(scores, vec![Some(1.0), Some(0.5), Some(0.0)]);
    }

    #[test]
    fn normalized_scoring_higher_is_better() {
        let scores = normalize_metric_scores(
            &[Some(10.0), Some(20.0), Some(30.0)],
            MetricDirection::HigherIsBetter,
        );

        assert_eq!(scores, vec![Some(0.0), Some(0.5), Some(1.0)]);
    }

    #[test]
    fn equal_values_score_neutral() {
        let scores = normalize_metric_scores(
            &[Some(4.0), Some(4.0), None],
            MetricDirection::LowerIsBetter,
        );

        assert_eq!(scores, vec![Some(0.5), Some(0.5), None]);
    }

    #[test]
    fn overall_score_redistributes_weight_when_metric_is_missing() {
        let mut scores = HashMap::new();
        scores.insert(BenchmarkMetric::Wer, 1.0);
        scores.insert(BenchmarkMetric::Wip, 0.0);

        let overall = calculate_overall_score(&scores, RankingMode::Accuracy).unwrap();

        assert!((overall - (0.60 / 0.70)).abs() < 0.0001);
    }
}
