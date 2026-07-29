#![allow(dead_code)]

#[path = "../src/models.rs"]
mod models;
#[path = "../src/voice_editor.rs"]
mod voice_editor;

use models::TranscriptResult;
use voice_editor::{VoiceEditDecision, finalize_voice_edits, plan_voice_edits};

fn transcript(text: &str) -> TranscriptResult {
    TranscriptResult {
        model_id: "release-corpus".to_owned(),
        model_name: "Release corpus".to_owned(),
        backend: "fake".to_owned(),
        text: text.to_owned(),
        segments: Vec::new(),
        duration_ms: None,
        stdout: String::new(),
        stderr: String::new(),
    }
}

#[test]
fn ordinary_release_corpus_has_zero_destructive_false_positives() {
    let corpus = [
        "The scratch thatch needs another coat of paint.",
        "We should start overtime after the planning meeting.",
        "The newlineage marker belongs in the migration notes.",
        "Undo thatched roof damage before winter.",
        "Replacement parts ship with the next order.",
        "Make thatched panels only for the garden shed.",
        "Rewrite thatched labels after the archive is scanned.",
        "Turn that interval into a weekly reporting cadence.",
        "Literal language can still appear in ordinary dictation.",
        "Please review the draft, shorten the title, and send it tomorrow.",
        "The customer asked for different paragraph styling in the document editor.",
        "I need an additional line item in the quarterly budget.",
        "The phrase starts over there beside the second column.",
        "Replaceable filters come with a two year warranty.",
        "Making that decision requires the full incident timeline.",
        "Rewriting that section manually will take another hour.",
        "Turning that idea into a prototype is the next milestone.",
        "The undo button should remain disabled in read only mode.",
        "A scratched surface should be photographed before repair.",
        "The paragraph starts on the following page.",
    ];

    for text in corpus {
        let plan = plan_voice_edits(&transcript(text));
        assert!(!plan.has_commands(), "false command match in {text:?}");
        let outcome = finalize_voice_edits(&plan, &[]);
        assert_eq!(outcome.edited_text, text);
        assert!(outcome.operations.is_empty());
        assert!(!outcome.requires_review);
        assert!(!outcome.used_ai);
    }
}

#[test]
fn deterministic_command_release_corpus_matches_expected_results() {
    let corpus = [
        ("Keep this. Remove this scratch that", "Keep this."),
        ("Keep this. Remove this. SCRATCH THAT!", "Keep this."),
        (
            "Keep this. Remove this scratch that undo that",
            "Keep this. Remove this",
        ),
        ("Discard this start over Begin again.", "Begin again."),
        ("First line new line second line", "First line\nsecond line"),
        (
            "First paragraph new paragraph second paragraph",
            "First paragraph\n\nsecond paragraph",
        ),
        (
            "Cats are calm. Cats are loud. Replace cats with dogs.",
            "Cats are calm. dogs are loud.",
        ),
        (
            "Say literal scratch that and literal new paragraph here.",
            "Say scratch that and new paragraph here.",
        ),
        (
            "First. new paragraph Second. replace Second with Final.",
            "First.\n\nFinal.",
        ),
    ];

    for (spoken, expected) in corpus {
        let plan = plan_voice_edits(&transcript(spoken));
        assert!(plan.has_commands(), "missed command in {spoken:?}");
        assert!(!plan.requires_ai(), "unexpected AI candidate in {spoken:?}");
        let outcome = finalize_voice_edits(&plan, &[]);
        assert!(!outcome.requires_review, "unexpected review in {spoken:?}");
        assert_eq!(outcome.edited_text, expected, "wrong edit for {spoken:?}");
    }
}

#[test]
fn explicit_rewrite_release_corpus_has_complete_candidate_recognition() {
    let corpus = [
        ("This is too long. Make that shorter.", "shorter"),
        ("This is indirect. rewrite that more direct.", "more direct"),
        (
            "A rough note. TURN THAT INTO a professional sentence!",
            "a professional sentence",
        ),
        ("Draft. Make that concise. Rewrite that warmer.", "concise"),
    ];
    let mut recognized = 0_usize;

    for (spoken, first_instruction) in corpus {
        let plan = plan_voice_edits(&transcript(spoken));
        if plan.has_commands() && plan.requires_ai() && !plan.candidates().is_empty() {
            recognized += 1;
        }
        assert_eq!(plan.candidates()[0].instruction, first_instruction);
    }

    let recognition = recognized as f64 / corpus.len() as f64;
    assert!(
        recognition >= 0.95,
        "recognition rate was {:.1}%",
        recognition * 100.0
    );
}

#[test]
fn release_corpus_routes_invalid_and_excessive_operations_to_review() {
    for spoken in [
        "Scratch that.",
        "Undo that.",
        "Replace missing text with present text.",
        "Rewrite that shorter.",
    ] {
        let plan = plan_voice_edits(&transcript(spoken));
        let outcome = finalize_voice_edits(&plan, &[]);
        assert!(
            outcome.requires_review,
            "review was not required for {spoken:?}"
        );
        assert_eq!(outcome.edited_text, spoken);
    }

    let mut excessive = String::from("Base");
    for _ in 0..=voice_editor::MAX_VOICE_EDIT_OPERATIONS {
        excessive.push_str(" new line next");
    }
    let outcome = finalize_voice_edits(&plan_voice_edits(&transcript(&excessive)), &[]);
    assert!(outcome.requires_review);
    assert_eq!(outcome.edited_text, excessive);
}

#[test]
fn rewrite_decisions_cannot_target_an_unknown_candidate() {
    let plan = plan_voice_edits(&transcript("Draft. Rewrite that shorter."));
    let outcome = finalize_voice_edits(
        &plan,
        &[VoiceEditDecision::ApplyRewrite {
            candidate_id: 99,
            replacement_text: "Brief.".to_owned(),
        }],
    );
    assert!(outcome.requires_review);
    assert_eq!(outcome.edited_text, "Draft. Rewrite that shorter.");
}
