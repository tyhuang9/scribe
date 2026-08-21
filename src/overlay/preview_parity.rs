use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreviewInput {
    Message,
    Error,
    Notice,
    TranscriptShort,
    TranscriptExactFit,
    TranscriptOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetainedText {
    Head,
    Full,
    Tail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum HorizontalAnchor {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PreviewParityCase {
    pub name: &'static str,
    pub input: PreviewInput,
    pub retained: RetainedText,
    pub anchor: HorizontalAnchor,
    pub ellipsis: bool,
}

pub(super) const PREVIEW_PARITY_CASES: [PreviewParityCase; 6] = [
    PreviewParityCase {
        name: "message-head-fit",
        input: PreviewInput::Message,
        retained: RetainedText::Head,
        anchor: HorizontalAnchor::Left,
        ellipsis: true,
    },
    PreviewParityCase {
        name: "error-head-fit",
        input: PreviewInput::Error,
        retained: RetainedText::Head,
        anchor: HorizontalAnchor::Left,
        ellipsis: true,
    },
    PreviewParityCase {
        name: "notice-head-fit",
        input: PreviewInput::Notice,
        retained: RetainedText::Head,
        anchor: HorizontalAnchor::Left,
        ellipsis: true,
    },
    PreviewParityCase {
        name: "transcript-short",
        input: PreviewInput::TranscriptShort,
        retained: RetainedText::Full,
        anchor: HorizontalAnchor::Left,
        ellipsis: false,
    },
    PreviewParityCase {
        name: "transcript-exact-fit",
        input: PreviewInput::TranscriptExactFit,
        retained: RetainedText::Full,
        anchor: HorizontalAnchor::Left,
        ellipsis: false,
    },
    PreviewParityCase {
        name: "transcript-overflow",
        input: PreviewInput::TranscriptOverflow,
        retained: RetainedText::Tail,
        anchor: HorizontalAnchor::Right,
        ellipsis: false,
    },
];

pub(super) const PARITY_GRAPHEMES: &str =
    "e\u{301} \u{6f22}\u{5b57} \u{1f1fa}\u{1f1f8} \u{1f44d}\u{1f3fd} \u{1f469}\u{200d}\u{1f4bb}";

pub(super) fn long_message(input: PreviewInput) -> String {
    let label = match input {
        PreviewInput::Message => "Preparing",
        PreviewInput::Error => "Microphone error",
        PreviewInput::Notice => "Preview notice",
        _ => panic!("{input:?} is not a message parity case"),
    };
    format!("{label}: {PARITY_GRAPHEMES} {PARITY_GRAPHEMES} remains readable")
}

pub(super) fn assert_text_contract(case: PreviewParityCase, original: &str, rendered: &str) {
    let retained = rendered.strip_suffix('\u{2026}').unwrap_or(rendered);
    assert_eq!(
        rendered.ends_with('\u{2026}'),
        case.ellipsis,
        "{} ellipsis contract",
        case.name
    );
    match case.retained {
        RetainedText::Head => assert!(
            original.starts_with(retained),
            "{} must retain the message head: {rendered:?}",
            case.name
        ),
        RetainedText::Full => assert_eq!(
            rendered, original,
            "{} must retain the complete text",
            case.name
        ),
        RetainedText::Tail => assert!(
            original.ends_with(retained),
            "{} must retain the transcript tail: {rendered:?}",
            case.name
        ),
    }

    let boundary = match case.retained {
        RetainedText::Head | RetainedText::Full => retained.len(),
        RetainedText::Tail => original.len().saturating_sub(retained.len()),
    };
    assert!(
        boundary == 0
            || boundary == original.len()
            || original
                .grapheme_indices(true)
                .any(|(index, _)| index == boundary),
        "{} split a grapheme cluster at byte {boundary}: {rendered:?}",
        case.name
    );
}
