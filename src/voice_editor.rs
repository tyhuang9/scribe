use std::collections::{HashMap, HashSet};

use crate::models::{TranscriptResult, TranscriptSegment};

pub(crate) const MAX_VOICE_EDIT_OPERATIONS: usize = 20;
pub(crate) const MAX_REWRITE_INSTRUCTION_BYTES: usize = 1_024;
pub(crate) const MAX_REWRITE_OUTPUT_BYTES: usize = 16 * 1_024;
pub(crate) const MAX_EDITED_TEXT_BYTES: usize = 256 * 1_024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VoiceEditBreak {
    Line,
    Paragraph,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VoiceEditOperation {
    ScratchThat {
        removed_text: String,
    },
    UndoThat,
    StartOver {
        removed_units: usize,
    },
    InsertBreak(VoiceEditBreak),
    Replace {
        from: String,
        to: String,
    },
    Rewrite {
        candidate_id: u32,
        before: String,
        after: String,
        instruction: String,
    },
    Literal {
        text: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceEditUnit {
    pub id: u32,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceEditCandidate {
    pub id: u32,
    pub target_unit_id: u32,
    pub target_text: String,
    pub instruction: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VoiceEditDecision {
    ApplyRewrite {
        candidate_id: u32,
        replacement_text: String,
    },
    NoChange {
        candidate_id: u32,
    },
    RequireReview {
        candidate_id: u32,
        reason: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VoiceEditOutcome {
    pub original_text: String,
    pub edited_text: String,
    pub operations: Vec<VoiceEditOperation>,
    pub used_ai: bool,
    pub warnings: Vec<String>,
    pub requires_review: bool,
}

impl VoiceEditOutcome {
    pub fn unchanged(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            original_text: text.clone(),
            edited_text: text,
            operations: Vec::new(),
            used_ai: false,
            warnings: Vec::new(),
            requires_review: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VoiceEditPlan {
    original_text: String,
    steps: Vec<ProgramStep>,
    candidates: Vec<VoiceEditCandidate>,
    command_count: usize,
    parser_warnings: Vec<String>,
}

impl VoiceEditPlan {
    pub fn candidates(&self) -> &[VoiceEditCandidate] {
        &self.candidates
    }

    pub fn has_commands(&self) -> bool {
        self.command_count > 0
    }

    pub fn requires_ai(&self) -> bool {
        !self.candidates.is_empty()
    }

    pub fn requires_review(&self) -> bool {
        self.command_count > MAX_VOICE_EDIT_OPERATIONS
            || self
                .steps
                .iter()
                .any(|step| matches!(step, ProgramStep::Review(_)))
    }

    pub fn candidate_with_context(
        &self,
        candidate_id: u32,
        decisions: &[VoiceEditDecision],
    ) -> Result<VoiceEditCandidate, String> {
        if self.requires_review() {
            return Err("Voice edit plan already requires review".to_owned());
        }
        let decision_map = decision_map(decisions)?;
        let mut evaluation = Evaluation::default();
        for step in &self.steps {
            if let ProgramStep::Rewrite(candidate) = step {
                if candidate.id == candidate_id {
                    let target = self
                        .candidates
                        .iter()
                        .find(|candidate| candidate.id == candidate_id)
                        .ok_or_else(|| format!("Unknown voice edit candidate {candidate_id}"))?;
                    let current_target = evaluation
                        .atoms
                        .iter()
                        .find_map(|atom| match atom {
                            BufferAtom::Text(unit) if unit.id == target.target_unit_id => {
                                Some(unit.text.clone())
                            }
                            BufferAtom::Text(_) | BufferAtom::Break(_) => None,
                        })
                        .ok_or_else(|| {
                            format!("Voice edit candidate {candidate_id} no longer has a target")
                        })?;
                    let mut contextual = target.clone();
                    contextual.target_text = current_target;
                    return Ok(contextual);
                }
                if !decision_map.contains_key(&candidate.id) {
                    return Err(format!(
                        "Voice edit candidate {} must be decided before candidate {candidate_id}",
                        candidate.id
                    ));
                }
            }
            evaluation.apply_step(step, &decision_map);
            if evaluation.requires_review {
                return Err(evaluation
                    .warnings
                    .last()
                    .cloned()
                    .unwrap_or_else(|| "Earlier voice edit requires review".to_owned()));
            }
        }
        Err(format!("Unknown voice edit candidate {candidate_id}"))
    }
}

#[derive(Clone, Debug)]
enum ProgramStep {
    Append(VoiceEditUnit),
    Literal(VoiceEditUnit),
    ScratchThat,
    UndoThat,
    StartOver,
    InsertBreak(VoiceEditBreak),
    Replace { from: String, to: String },
    Rewrite(VoiceEditCandidate),
    Review(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum BufferAtom {
    Text(VoiceEditUnit),
    Break(VoiceEditBreak),
}

#[derive(Default)]
struct Evaluation {
    atoms: Vec<BufferAtom>,
    history: Vec<Vec<BufferAtom>>,
    operations: Vec<VoiceEditOperation>,
    warnings: Vec<String>,
    requires_review: bool,
    used_ai: bool,
}

pub(crate) fn plan_voice_edits(result: &TranscriptResult) -> VoiceEditPlan {
    plan_voice_edits_from_parts(&result.text, &result.segments)
}

pub(crate) fn finalize_voice_edits(
    plan: &VoiceEditPlan,
    decisions: &[VoiceEditDecision],
) -> VoiceEditOutcome {
    if !plan.has_commands() {
        return VoiceEditOutcome::unchanged(plan.original_text.clone());
    }

    let mut evaluation = Evaluation {
        warnings: plan.parser_warnings.clone(),
        ..Evaluation::default()
    };
    let decision_map = match decision_map(decisions) {
        Ok(decision_map) => decision_map,
        Err(message) => {
            evaluation.requires_review = true;
            evaluation.warnings.push(message);
            HashMap::new()
        }
    };

    let known_candidates = plan
        .candidates
        .iter()
        .map(|candidate| candidate.id)
        .collect::<HashSet<_>>();
    for candidate_id in decision_map.keys() {
        if !known_candidates.contains(candidate_id) {
            evaluation.requires_review = true;
            evaluation.warnings.push(format!(
                "Voice edit response referenced unknown candidate {candidate_id}"
            ));
        }
    }

    if plan.command_count > MAX_VOICE_EDIT_OPERATIONS {
        evaluation.requires_review = true;
        evaluation.warnings.push(format!(
            "Voice editing found {} operations; the limit is {MAX_VOICE_EDIT_OPERATIONS}",
            plan.command_count
        ));
    }

    for step in &plan.steps {
        evaluation.apply_step(step, &decision_map);
    }

    let mut edited_text = render_atoms(&evaluation.atoms);
    if edited_text.len() > MAX_EDITED_TEXT_BYTES {
        evaluation.requires_review = true;
        evaluation.warnings.push(format!(
            "Edited transcript exceeds the {MAX_EDITED_TEXT_BYTES}-byte limit"
        ));
    }
    if evaluation.requires_review {
        edited_text = plan.original_text.clone();
    }

    VoiceEditOutcome {
        original_text: plan.original_text.clone(),
        edited_text,
        operations: evaluation.operations,
        used_ai: evaluation.used_ai,
        warnings: evaluation.warnings,
        requires_review: evaluation.requires_review,
    }
}

fn decision_map(
    decisions: &[VoiceEditDecision],
) -> Result<HashMap<u32, &VoiceEditDecision>, String> {
    let mut decision_map = HashMap::new();
    for decision in decisions {
        let candidate_id = match decision {
            VoiceEditDecision::ApplyRewrite { candidate_id, .. }
            | VoiceEditDecision::NoChange { candidate_id }
            | VoiceEditDecision::RequireReview { candidate_id, .. } => *candidate_id,
        };
        if decision_map.insert(candidate_id, decision).is_some() {
            return Err(format!(
                "Voice edit candidate {candidate_id} received more than one decision"
            ));
        }
    }
    Ok(decision_map)
}

fn plan_voice_edits_from_parts(text: &str, segments: &[TranscriptSegment]) -> VoiceEditPlan {
    let source_units = source_units(text, segments);
    let mut steps = Vec::new();
    let mut candidates = Vec::new();
    let mut parser_warnings = Vec::new();
    let mut next_unit_id = 1_u32;
    let mut next_candidate_id = 1_u32;
    let mut command_count = 0_usize;
    let mut simulation = Evaluation::default();

    for source in source_units {
        let mut cursor = 0;
        while let Some(trigger) = next_trigger(&source.text, cursor) {
            append_plain_span(
                &mut steps,
                &mut simulation,
                &source,
                &source.text[cursor..trigger.start],
                &mut next_unit_id,
            );

            match trigger.kind {
                TriggerKind::Literal(literal_text) => {
                    command_count += 1;
                    let unit = VoiceEditUnit {
                        id: next_unit_id,
                        start_ms: source.start_ms,
                        end_ms: source.end_ms,
                        text: literal_text,
                    };
                    next_unit_id += 1;
                    let step = ProgramStep::Literal(unit);
                    simulation.apply_step(&step, &HashMap::new());
                    steps.push(step);
                }
                TriggerKind::ScratchThat => {
                    command_count += 1;
                    push_simulated_step(&mut steps, &mut simulation, ProgramStep::ScratchThat);
                }
                TriggerKind::UndoThat => {
                    command_count += 1;
                    push_simulated_step(&mut steps, &mut simulation, ProgramStep::UndoThat);
                }
                TriggerKind::StartOver => {
                    command_count += 1;
                    push_simulated_step(&mut steps, &mut simulation, ProgramStep::StartOver);
                }
                TriggerKind::NewLine => {
                    command_count += 1;
                    push_simulated_step(
                        &mut steps,
                        &mut simulation,
                        ProgramStep::InsertBreak(VoiceEditBreak::Line),
                    );
                }
                TriggerKind::NewParagraph => {
                    command_count += 1;
                    push_simulated_step(
                        &mut steps,
                        &mut simulation,
                        ProgramStep::InsertBreak(VoiceEditBreak::Paragraph),
                    );
                }
                TriggerKind::Replace { from, to } => {
                    command_count += 1;
                    if from.is_empty() || to.is_empty() {
                        let message =
                            "Replace command requires text before and after 'with'".to_owned();
                        parser_warnings.push(message.clone());
                        steps.push(ProgramStep::Review(message));
                    } else {
                        push_simulated_step(
                            &mut steps,
                            &mut simulation,
                            ProgramStep::Replace { from, to },
                        );
                    }
                }
                TriggerKind::Rewrite { instruction } => {
                    command_count += 1;
                    let instruction = instruction.trim().to_owned();
                    let target = last_text_unit(&simulation.atoms).cloned();
                    if instruction.is_empty() {
                        let message = "Rewrite command requires an instruction".to_owned();
                        parser_warnings.push(message.clone());
                        steps.push(ProgramStep::Review(message));
                    } else if instruction.len() > MAX_REWRITE_INSTRUCTION_BYTES {
                        let message = format!(
                            "Rewrite instruction exceeds the {MAX_REWRITE_INSTRUCTION_BYTES}-byte limit"
                        );
                        parser_warnings.push(message.clone());
                        steps.push(ProgramStep::Review(message));
                    } else if let Some(target) = target {
                        let candidate = VoiceEditCandidate {
                            id: next_candidate_id,
                            target_unit_id: target.id,
                            target_text: target.text.clone(),
                            instruction,
                        };
                        next_candidate_id += 1;
                        candidates.push(candidate.clone());
                        let step = ProgramStep::Rewrite(candidate);
                        simulation.apply_step(&step, &HashMap::new());
                        steps.push(step);
                    } else {
                        let message = "Rewrite command has no preceding edit unit".to_owned();
                        parser_warnings.push(message.clone());
                        steps.push(ProgramStep::Review(message));
                    }
                }
                TriggerKind::Malformed(message) => {
                    command_count += 1;
                    parser_warnings.push(message.clone());
                    steps.push(ProgramStep::Review(message));
                }
            }
            cursor = trigger.end;
        }

        append_plain_span(
            &mut steps,
            &mut simulation,
            &source,
            &source.text[cursor..],
            &mut next_unit_id,
        );
    }

    VoiceEditPlan {
        original_text: text.to_owned(),
        steps,
        candidates,
        command_count,
        parser_warnings,
    }
}

fn push_simulated_step(
    steps: &mut Vec<ProgramStep>,
    simulation: &mut Evaluation,
    step: ProgramStep,
) {
    simulation.apply_step(&step, &HashMap::new());
    steps.push(step);
}

fn append_plain_span(
    steps: &mut Vec<ProgramStep>,
    simulation: &mut Evaluation,
    source: &VoiceEditUnit,
    span: &str,
    next_unit_id: &mut u32,
) {
    let text = trim_command_spacing(span);
    if text.is_empty() {
        return;
    }
    let unit = VoiceEditUnit {
        id: *next_unit_id,
        start_ms: source.start_ms,
        end_ms: source.end_ms,
        text,
    };
    *next_unit_id += 1;
    let step = ProgramStep::Append(unit);
    simulation.apply_step(&step, &HashMap::new());
    steps.push(step);
}

impl Evaluation {
    fn apply_step(&mut self, step: &ProgramStep, decisions: &HashMap<u32, &VoiceEditDecision>) {
        match step {
            ProgramStep::Append(unit) => self.atoms.push(BufferAtom::Text(unit.clone())),
            ProgramStep::Literal(unit) => {
                self.atoms.push(BufferAtom::Text(unit.clone()));
                self.operations.push(VoiceEditOperation::Literal {
                    text: unit.text.clone(),
                });
            }
            ProgramStep::ScratchThat => {
                if let Some(index) = self
                    .atoms
                    .iter()
                    .rposition(|atom| matches!(atom, BufferAtom::Text(_)))
                {
                    self.history.push(self.atoms.clone());
                    let removed_text = match self.atoms.remove(index) {
                        BufferAtom::Text(unit) => unit.text,
                        BufferAtom::Break(_) => unreachable!(),
                    };
                    self.trim_orphaned_breaks();
                    self.operations
                        .push(VoiceEditOperation::ScratchThat { removed_text });
                } else {
                    self.review("Scratch command has no preceding edit unit");
                }
            }
            ProgramStep::UndoThat => {
                if let Some(previous) = self.history.pop() {
                    self.atoms = previous;
                    self.operations.push(VoiceEditOperation::UndoThat);
                } else {
                    self.review("Undo command has no preceding in-recording operation");
                }
            }
            ProgramStep::StartOver => {
                if self.atoms.is_empty() {
                    self.review("Start-over command has no current-recording text to clear");
                } else {
                    let removed_units = self
                        .atoms
                        .iter()
                        .filter(|atom| matches!(atom, BufferAtom::Text(_)))
                        .count();
                    self.history.push(self.atoms.clone());
                    self.atoms.clear();
                    self.operations
                        .push(VoiceEditOperation::StartOver { removed_units });
                }
            }
            ProgramStep::InsertBreak(kind) => {
                if self.atoms.is_empty() {
                    self.review("Line-break command has no preceding edit unit");
                } else {
                    self.history.push(self.atoms.clone());
                    while matches!(self.atoms.last(), Some(BufferAtom::Break(_))) {
                        self.atoms.pop();
                    }
                    self.atoms.push(BufferAtom::Break(*kind));
                    self.operations.push(VoiceEditOperation::InsertBreak(*kind));
                }
            }
            ProgramStep::Replace { from, to } => {
                let before = self.atoms.clone();
                if replace_most_recent(&mut self.atoms, from, to) {
                    self.history.push(before);
                    self.operations.push(VoiceEditOperation::Replace {
                        from: from.clone(),
                        to: to.clone(),
                    });
                } else {
                    self.review(format!("Replace target was not found: {from:?}"));
                }
            }
            ProgramStep::Rewrite(candidate) => match decisions.get(&candidate.id) {
                Some(VoiceEditDecision::ApplyRewrite {
                    replacement_text, ..
                }) => {
                    self.used_ai = true;
                    if let Err(message) = validate_rewrite_text(replacement_text) {
                        self.review(message);
                        return;
                    }
                    let Some(index) = self.atoms.iter().position(|atom| {
                        matches!(atom, BufferAtom::Text(unit) if unit.id == candidate.target_unit_id)
                    }) else {
                        self.review(format!(
                            "Rewrite candidate {} no longer has a valid target",
                            candidate.id
                        ));
                        return;
                    };
                    self.history.push(self.atoms.clone());
                    let before = match &mut self.atoms[index] {
                        BufferAtom::Text(unit) => {
                            let before = unit.text.clone();
                            unit.text = replacement_text.trim().to_owned();
                            before
                        }
                        BufferAtom::Break(_) => unreachable!(),
                    };
                    self.operations.push(VoiceEditOperation::Rewrite {
                        candidate_id: candidate.id,
                        before,
                        after: replacement_text.trim().to_owned(),
                        instruction: candidate.instruction.clone(),
                    });
                }
                Some(VoiceEditDecision::NoChange { .. }) => {
                    self.used_ai = true;
                }
                Some(VoiceEditDecision::RequireReview { reason, .. }) => {
                    self.used_ai = true;
                    self.review(format!(
                        "Rewrite candidate {} requires review: {}",
                        candidate.id,
                        bounded_message(reason)
                    ));
                }
                None => self.review(format!(
                    "Rewrite candidate {} has no local-AI decision",
                    candidate.id
                )),
            },
            ProgramStep::Review(message) => self.review(message.clone()),
        }
    }

    fn review(&mut self, message: impl Into<String>) {
        self.requires_review = true;
        self.warnings.push(message.into());
    }

    fn trim_orphaned_breaks(&mut self) {
        while matches!(self.atoms.last(), Some(BufferAtom::Break(_))) {
            self.atoms.pop();
        }
        while matches!(self.atoms.first(), Some(BufferAtom::Break(_))) {
            self.atoms.remove(0);
        }
    }
}

fn validate_rewrite_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Local-AI rewrite returned empty text".to_owned());
    }
    if text.len() > MAX_REWRITE_OUTPUT_BYTES {
        return Err(format!(
            "Local-AI rewrite exceeds the {MAX_REWRITE_OUTPUT_BYTES}-byte limit"
        ));
    }
    if text.contains('\0') {
        return Err("Local-AI rewrite contains a NUL character".to_owned());
    }
    Ok(())
}

fn bounded_message(message: &str) -> String {
    const MAX_MESSAGE_CHARS: usize = 240;
    let mut chars = message.chars();
    let bounded = chars.by_ref().take(MAX_MESSAGE_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn replace_most_recent(atoms: &mut Vec<BufferAtom>, from: &str, to: &str) -> bool {
    for atom in atoms.iter_mut().rev() {
        let BufferAtom::Text(unit) = atom else {
            continue;
        };
        if let Some(index) = rfind_ascii_case_insensitive(&unit.text, from) {
            unit.text.replace_range(index..index + from.len(), to);
            return true;
        }
    }

    let rendered = render_atoms(atoms);
    let Some(index) = rfind_ascii_case_insensitive(&rendered, from) else {
        return false;
    };
    let mut replaced = rendered;
    replaced.replace_range(index..index + from.len(), to);
    let stable_id = last_text_unit(atoms).map(|unit| unit.id).unwrap_or(1);
    *atoms = vec![BufferAtom::Text(VoiceEditUnit {
        id: stable_id,
        start_ms: None,
        end_ms: None,
        text: replaced,
    })];
    true
}

fn render_atoms(atoms: &[BufferAtom]) -> String {
    let mut rendered = String::new();
    for atom in atoms {
        match atom {
            BufferAtom::Text(unit) => append_rendered_text(&mut rendered, &unit.text),
            BufferAtom::Break(VoiceEditBreak::Line) => {
                trim_trailing_spaces(&mut rendered);
                if !rendered.ends_with('\n') {
                    rendered.push('\n');
                }
            }
            BufferAtom::Break(VoiceEditBreak::Paragraph) => {
                trim_trailing_spaces(&mut rendered);
                while rendered.ends_with('\n') {
                    rendered.pop();
                }
                rendered.push_str("\n\n");
            }
        }
    }
    rendered.trim().to_owned()
}

fn append_rendered_text(rendered: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !rendered.is_empty()
        && !rendered.ends_with(char::is_whitespace)
        && !starts_with_closing_punctuation(text)
    {
        rendered.push(' ');
    }
    rendered.push_str(text);
}

fn starts_with_closing_punctuation(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|ch| matches!(ch, '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}'))
}

fn trim_trailing_spaces(text: &mut String) {
    while text.ends_with([' ', '\t']) {
        text.pop();
    }
}

fn last_text_unit(atoms: &[BufferAtom]) -> Option<&VoiceEditUnit> {
    atoms.iter().rev().find_map(|atom| match atom {
        BufferAtom::Text(unit) => Some(unit),
        BufferAtom::Break(_) => None,
    })
}

fn source_units(text: &str, segments: &[TranscriptSegment]) -> Vec<VoiceEditUnit> {
    let has_timing = segments
        .iter()
        .any(|segment| segment.start_ms.is_some() || segment.end_ms.is_some());
    if has_timing {
        let units = segments
            .iter()
            .enumerate()
            .filter_map(|(index, segment)| {
                let text = segment.text.trim();
                (!text.is_empty()).then(|| VoiceEditUnit {
                    id: index as u32 + 1,
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                    text: text.to_owned(),
                })
            })
            .collect::<Vec<_>>();
        if !units.is_empty() {
            return units;
        }
    }

    split_fallback_units(text)
}

fn split_fallback_units(text: &str) -> Vec<VoiceEditUnit> {
    let mut units = Vec::new();
    let mut start = 0;
    let mut next_id = 1_u32;
    for (index, ch) in text.char_indices() {
        if !matches!(ch, '.' | '!' | '?' | ';' | ':') {
            continue;
        }
        let end = index + ch.len_utf8();
        push_fallback_unit(&mut units, &text[start..end], &mut next_id);
        start = end;
    }
    push_fallback_unit(&mut units, &text[start..], &mut next_id);
    units
}

fn push_fallback_unit(units: &mut Vec<VoiceEditUnit>, text: &str, next_id: &mut u32) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    units.push(VoiceEditUnit {
        id: *next_id,
        start_ms: None,
        end_ms: None,
        text: text.to_owned(),
    });
    *next_id += 1;
}

#[derive(Clone, Debug)]
struct TriggerMatch {
    start: usize,
    end: usize,
    kind: TriggerKind,
}

#[derive(Clone, Debug)]
enum TriggerKind {
    Literal(String),
    ScratchThat,
    UndoThat,
    StartOver,
    NewLine,
    NewParagraph,
    Replace { from: String, to: String },
    Rewrite { instruction: String },
    Malformed(String),
}

fn next_trigger(text: &str, from: usize) -> Option<TriggerMatch> {
    text.char_indices()
        .filter(|(index, _)| *index >= from)
        .find_map(|(index, _)| trigger_at(text, index))
}

fn trigger_at(text: &str, index: usize) -> Option<TriggerMatch> {
    if let Some(literal_end) = match_word(text, index, "literal") {
        let command_start = skip_ascii_whitespace(text, literal_end);
        if command_start > literal_end {
            if let Some(command) = command_at(text, command_start) {
                let literal = trim_command_spacing(&text[command_start..command.end]);
                return Some(TriggerMatch {
                    start: index,
                    end: command.end,
                    kind: TriggerKind::Literal(literal),
                });
            }
        }
    }
    command_at(text, index)
}

fn command_at(text: &str, index: usize) -> Option<TriggerMatch> {
    for (phrase, kind) in [
        ("new paragraph", TriggerKind::NewParagraph),
        ("scratch that", TriggerKind::ScratchThat),
        ("start over", TriggerKind::StartOver),
        ("undo that", TriggerKind::UndoThat),
        ("new line", TriggerKind::NewLine),
    ] {
        if let Some(end) = match_phrase(text, index, phrase) {
            return Some(TriggerMatch {
                start: index,
                end,
                kind,
            });
        }
    }

    if let Some(prefix_end) = match_word(text, index, "replace") {
        let payload_start = skip_ascii_whitespace(text, prefix_end);
        if payload_start == prefix_end {
            return None;
        }
        let end = command_payload_end(text, payload_start);
        let payload = trim_payload(&text[payload_start..end]);
        if let Some(with_index) = find_word_ascii_case_insensitive(&payload, "with") {
            let from = trim_payload(&payload[..with_index]);
            let to = trim_payload(&payload[with_index + "with".len()..]);
            return Some(TriggerMatch {
                start: index,
                end,
                kind: TriggerKind::Replace { from, to },
            });
        }
        return Some(TriggerMatch {
            start: index,
            end,
            kind: TriggerKind::Malformed(
                "Replace command requires the form 'replace X with Y'".to_owned(),
            ),
        });
    }

    for phrase in ["turn that into", "rewrite that", "make that"] {
        if let Some(prefix_end) = match_phrase(text, index, phrase) {
            let payload_start = skip_ascii_whitespace(text, prefix_end);
            let end = command_payload_end(text, payload_start);
            return Some(TriggerMatch {
                start: index,
                end,
                kind: TriggerKind::Rewrite {
                    instruction: trim_payload(&text[payload_start..end]),
                },
            });
        }
    }
    None
}

fn match_word(text: &str, index: usize, word: &str) -> Option<usize> {
    match_phrase(text, index, word)
}

fn match_phrase(text: &str, index: usize, phrase: &str) -> Option<usize> {
    if !is_start_boundary(text, index) {
        return None;
    }
    let end = index.checked_add(phrase.len())?;
    let slice = text.get(index..end)?;
    if !slice.eq_ignore_ascii_case(phrase) || !is_end_boundary(text, end) {
        return None;
    }
    Some(end)
}

fn is_start_boundary(text: &str, index: usize) -> bool {
    index == 0
        || text[..index]
            .chars()
            .next_back()
            .is_none_or(|ch| !is_word_character(ch))
}

fn is_end_boundary(text: &str, index: usize) -> bool {
    index == text.len()
        || text[index..]
            .chars()
            .next()
            .is_none_or(|ch| !is_word_character(ch))
}

fn is_word_character(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn skip_ascii_whitespace(text: &str, mut index: usize) -> usize {
    while let Some(byte) = text.as_bytes().get(index) {
        if !byte.is_ascii_whitespace() {
            break;
        }
        index += 1;
    }
    index
}

fn command_payload_end(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .find_map(|(offset, ch)| {
            matches!(ch, '.' | '!' | '?' | ';').then_some(start + offset + ch.len_utf8())
        })
        .unwrap_or(text.len())
}

fn trim_command_spacing(text: &str) -> String {
    text.trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, ','))
        .to_owned()
}

fn trim_payload(text: &str) -> String {
    text.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, ',' | '.' | '!' | '?' | ';' | ':')
    })
    .to_owned()
}

fn find_word_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack.char_indices().find_map(|(index, _)| {
        match_phrase(haystack, index, needle)
            .filter(|_| {
                is_start_boundary(haystack, index)
                    && is_end_boundary(haystack, index + needle.len())
            })
            .map(|_| index)
    })
}

fn rfind_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .char_indices()
        .map(|(index, _)| index)
        .filter(|index| {
            haystack
                .get(*index..index.saturating_add(needle.len()))
                .is_some_and(|slice| slice.eq_ignore_ascii_case(needle))
        })
        .last()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(text: &str) -> TranscriptResult {
        TranscriptResult {
            model_id: "test".to_owned(),
            model_name: "Test".to_owned(),
            backend: "test".to_owned(),
            text: text.to_owned(),
            segments: Vec::new(),
            duration_ms: None,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn edit(text: &str) -> VoiceEditOutcome {
        let plan = plan_voice_edits(&result(text));
        finalize_voice_edits(&plan, &[])
    }

    #[test]
    fn ordinary_dictation_is_unchanged_and_never_needs_ai() {
        let transcript = "Please send the draft after lunch.";
        let plan = plan_voice_edits(&result(transcript));

        assert!(!plan.has_commands());
        assert!(!plan.requires_ai());
        assert!(!plan.requires_review());
        assert_eq!(finalize_voice_edits(&plan, &[]).edited_text, transcript);
    }

    #[test]
    fn scratch_that_removes_the_preceding_fallback_unit() {
        let outcome = edit("Keep this. Remove this. Scratch that Continue here.");

        assert_eq!(outcome.edited_text, "Keep this. Continue here.");
        assert_eq!(
            outcome.operations,
            vec![VoiceEditOperation::ScratchThat {
                removed_text: "Remove this.".to_owned()
            }]
        );
    }

    #[test]
    fn scratch_that_uses_timed_segments_as_units() {
        let mut transcript = result("First thought second thought scratch that final thought");
        transcript.segments = vec![
            TranscriptSegment {
                start_ms: Some(0),
                end_ms: Some(800),
                text: "First thought".to_owned(),
            },
            TranscriptSegment {
                start_ms: Some(800),
                end_ms: Some(1_600),
                text: "second thought scratch that final thought".to_owned(),
            },
        ];

        let plan = plan_voice_edits(&transcript);
        let outcome = finalize_voice_edits(&plan, &[]);
        assert_eq!(outcome.edited_text, "First thought final thought");
    }

    #[test]
    fn command_matching_is_case_insensitive_and_punctuation_tolerant() {
        let outcome = edit("Keep this. REMOVE THIS! SCRATCH THAT, Continue.");
        assert_eq!(outcome.edited_text, "Keep this. Continue.");
        assert!(!outcome.requires_review);
    }

    #[test]
    fn command_substrings_do_not_match() {
        for transcript in [
            "The scratch thatch needs paint.",
            "We will start overtime tomorrow.",
            "Use a newlineage marker.",
        ] {
            let plan = plan_voice_edits(&result(transcript));
            assert!(!plan.has_commands(), "unexpected command in {transcript:?}");
        }
    }

    #[test]
    fn literal_escape_dictates_reserved_phrases() {
        let outcome = edit("Write literal scratch that and literal new paragraph here.");
        assert_eq!(
            outcome.edited_text,
            "Write scratch that and new paragraph here."
        );
        assert!(!outcome.requires_review);
        assert_eq!(outcome.operations.len(), 2);
    }

    #[test]
    fn literal_escape_supports_parameterized_commands() {
        let outcome = edit("Say literal replace cats with dogs. Then continue.");
        assert_eq!(
            outcome.edited_text,
            "Say replace cats with dogs. Then continue."
        );
        assert!(!outcome.requires_review);
    }

    #[test]
    fn start_over_clears_only_the_current_recording_buffer() {
        let outcome = edit("Discard all of this. Start over Keep only this.");
        assert_eq!(outcome.edited_text, "Keep only this.");
        assert_eq!(
            outcome.operations,
            vec![VoiceEditOperation::StartOver { removed_units: 1 }]
        );
    }

    #[test]
    fn undo_that_reverses_the_preceding_operation() {
        let outcome = edit("Keep one. Remove two. Scratch that undo that Finish.");
        assert_eq!(outcome.edited_text, "Keep one. Remove two. Finish.");
        assert_eq!(outcome.operations.len(), 2);
        assert!(matches!(
            outcome.operations[1],
            VoiceEditOperation::UndoThat
        ));
    }

    #[test]
    fn new_line_and_new_paragraph_insert_structure() {
        let line = edit("First line new line second line");
        assert_eq!(line.edited_text, "First line\nsecond line");

        let paragraph = edit("First paragraph new paragraph second paragraph");
        assert_eq!(paragraph.edited_text, "First paragraph\n\nsecond paragraph");
    }

    #[test]
    fn replace_changes_the_most_recent_matching_text() {
        let outcome = edit("Cats are good. Cats are loud. Replace cats with dogs.");
        assert_eq!(outcome.edited_text, "Cats are good. dogs are loud.");
        assert!(matches!(
            outcome.operations.as_slice(),
            [VoiceEditOperation::Replace { from, to }]
                if from.eq_ignore_ascii_case("cats") && to == "dogs"
        ));
    }

    #[test]
    fn replace_missing_target_requires_review_and_preserves_original() {
        let transcript = "Keep this. Replace cats with dogs.";
        let outcome = edit(transcript);
        assert!(outcome.requires_review);
        assert_eq!(outcome.edited_text, transcript);
    }

    #[test]
    fn malformed_replace_requires_review() {
        let transcript = "Keep this. Replace cats.";
        let outcome = edit(transcript);
        assert!(outcome.requires_review);
        assert_eq!(outcome.edited_text, transcript);
    }

    #[test]
    fn missing_targets_require_review_and_preserve_original() {
        for transcript in ["scratch that", "undo that", "start over", "new line"] {
            let outcome = edit(transcript);
            assert!(
                outcome.requires_review,
                "expected review for {transcript:?}"
            );
            assert_eq!(outcome.edited_text, transcript);
        }
    }

    #[test]
    fn rewrite_emits_one_bounded_ai_candidate() {
        let plan = plan_voice_edits(&result(
            "The release is soon. Rewrite that make it more direct.",
        ));

        assert!(plan.requires_ai());
        assert_eq!(plan.candidates().len(), 1);
        assert_eq!(plan.candidates()[0].target_text, "The release is soon.");
        assert_eq!(plan.candidates()[0].instruction, "make it more direct");
    }

    #[test]
    fn valid_rewrite_decision_updates_only_the_target_unit() {
        let plan = plan_voice_edits(&result(
            "The release is soon. Rewrite that make it more direct. Keep this.",
        ));
        let outcome = finalize_voice_edits(
            &plan,
            &[VoiceEditDecision::ApplyRewrite {
                candidate_id: 1,
                replacement_text: "Ship soon.".to_owned(),
            }],
        );

        assert_eq!(outcome.edited_text, "Ship soon. Keep this.");
        assert!(outcome.used_ai);
        assert!(!outcome.requires_review);
    }

    #[test]
    fn no_change_decision_records_ai_use_without_an_applied_operation() {
        let transcript = "Keep this. Rewrite that keep the wording if it is already clear.";
        let plan = plan_voice_edits(&result(transcript));
        let outcome =
            finalize_voice_edits(&plan, &[VoiceEditDecision::NoChange { candidate_id: 1 }]);

        assert_eq!(outcome.edited_text, "Keep this.");
        assert!(outcome.used_ai);
        assert!(outcome.operations.is_empty());
        assert!(!outcome.requires_review);
    }

    #[test]
    fn later_rewrite_candidate_uses_prior_validated_rewrite_text() {
        let plan = plan_voice_edits(&result(
            "This is wordy. Rewrite that make it concise. Rewrite that make it enthusiastic.",
        ));
        assert_eq!(plan.candidates().len(), 2);

        let first = VoiceEditDecision::ApplyRewrite {
            candidate_id: 1,
            replacement_text: "Brief.".to_owned(),
        };
        let second = plan
            .candidate_with_context(2, std::slice::from_ref(&first))
            .unwrap();
        assert_eq!(second.target_text, "Brief.");

        let outcome = finalize_voice_edits(
            &plan,
            &[
                first,
                VoiceEditDecision::ApplyRewrite {
                    candidate_id: 2,
                    replacement_text: "Brief and bold!".to_owned(),
                },
            ],
        );
        assert_eq!(outcome.edited_text, "Brief and bold!");
        assert_eq!(outcome.operations.len(), 2);
        assert!(!outcome.requires_review);
    }

    #[test]
    fn later_rewrite_candidate_rejects_missing_or_reviewed_prior_decision() {
        let plan = plan_voice_edits(&result(
            "This is wordy. Rewrite that make it concise. Rewrite that make it enthusiastic.",
        ));
        assert!(plan.candidate_with_context(2, &[]).is_err());
        assert!(
            plan.candidate_with_context(
                2,
                &[VoiceEditDecision::RequireReview {
                    candidate_id: 1,
                    reason: "ambiguous".to_owned(),
                }]
            )
            .is_err()
        );
    }

    #[test]
    fn missing_invalid_and_unknown_ai_decisions_fail_closed() {
        let transcript = "Keep this. Rewrite that make it shorter.";
        let plan = plan_voice_edits(&result(transcript));
        assert!(finalize_voice_edits(&plan, &[]).requires_review);

        let empty = finalize_voice_edits(
            &plan,
            &[VoiceEditDecision::ApplyRewrite {
                candidate_id: 1,
                replacement_text: "   ".to_owned(),
            }],
        );
        assert!(empty.requires_review);
        assert_eq!(empty.edited_text, transcript);

        let unknown = finalize_voice_edits(
            &plan,
            &[VoiceEditDecision::ApplyRewrite {
                candidate_id: 99,
                replacement_text: "Changed".to_owned(),
            }],
        );
        assert!(unknown.requires_review);
        assert_eq!(unknown.edited_text, transcript);
    }

    #[test]
    fn explicit_ai_review_preserves_the_original() {
        let transcript = "Keep this. Make that more concise.";
        let plan = plan_voice_edits(&result(transcript));
        let outcome = finalize_voice_edits(
            &plan,
            &[VoiceEditDecision::RequireReview {
                candidate_id: 1,
                reason: "ambiguous request".to_owned(),
            }],
        );
        assert!(outcome.requires_review);
        assert_eq!(outcome.edited_text, transcript);
    }

    #[test]
    fn multiple_commands_are_processed_sequentially() {
        let outcome = edit(
            "First. Wrong second. Scratch that New second. New paragraph Third. Replace third with final.",
        );
        assert_eq!(outcome.edited_text, "First. New second.\n\nfinal.");
        assert_eq!(outcome.operations.len(), 3);
    }

    #[test]
    fn deletion_to_empty_is_an_intentional_success() {
        let outcome = edit("Remove this scratch that");
        assert_eq!(outcome.edited_text, "");
        assert!(!outcome.requires_review);
    }

    #[test]
    fn more_than_twenty_operations_requires_review() {
        let mut transcript = String::from("Base");
        for _ in 0..=MAX_VOICE_EDIT_OPERATIONS {
            transcript.push_str(" new line next");
        }
        let outcome = edit(&transcript);
        assert!(outcome.requires_review);
        assert_eq!(outcome.edited_text, transcript);
    }

    #[test]
    fn fallback_units_split_on_sentence_and_clause_boundaries() {
        let units = split_fallback_units("One: two; three! Four?");
        assert_eq!(
            units
                .iter()
                .map(|unit| unit.text.as_str())
                .collect::<Vec<_>>(),
            vec!["One:", "two;", "three!", "Four?"]
        );
        assert!(units.iter().all(|unit| unit.start_ms.is_none()));
    }

    #[test]
    fn timed_source_units_preserve_timestamps() {
        let segments = vec![TranscriptSegment {
            start_ms: Some(125),
            end_ms: Some(875),
            text: "Timed text".to_owned(),
        }];
        let units = source_units("Timed text", &segments);
        assert_eq!(units[0].start_ms, Some(125));
        assert_eq!(units[0].end_ms, Some(875));
    }

    #[test]
    fn plans_do_not_share_state_between_recordings() {
        let first = edit("Old recording start over New text.");
        let second = edit("Second recording.");
        assert_eq!(first.edited_text, "New text.");
        assert_eq!(second.edited_text, "Second recording.");
        assert!(second.operations.is_empty());
    }

    #[test]
    fn unicode_text_around_ascii_commands_is_preserved() {
        let outcome = edit("Caf\u{00e9} draft. scratch that R\u{00e9}sum\u{00e9} final.");
        assert_eq!(outcome.edited_text, "R\u{00e9}sum\u{00e9} final.");
    }
}
