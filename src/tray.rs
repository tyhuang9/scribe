use anyhow::{Context, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_SHOW: &str = "scribe-show";
const MENU_HIDE: &str = "scribe-hide";
const MENU_TOGGLE_RECORDING: &str = "scribe-toggle-recording";
const MENU_COPY_LAST: &str = "scribe-copy-last";
const MENU_QUIT: &str = "scribe-quit";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    Show,
    Hide,
    ToggleRecording,
    CopyLastTranscript,
    Quit,
}

pub struct TrayService {
    _tray_icon: TrayIcon,
    toggle_recording_item: MenuItem,
    copy_last_item: MenuItem,
    _show_item: MenuItem,
    _hide_item: MenuItem,
    _quit_item: MenuItem,
    _separator: PredefinedMenuItem,
}

impl TrayService {
    pub fn new(is_recording: bool, has_transcript: bool) -> Result<Self> {
        let show_item = MenuItem::with_id(MENU_SHOW, "Show Scribe", true, None);
        let hide_item = MenuItem::with_id(MENU_HIDE, "Hide Window", true, None);
        let toggle_recording_item = MenuItem::with_id(
            MENU_TOGGLE_RECORDING,
            recording_label(is_recording),
            true,
            None,
        );
        let copy_last_item =
            MenuItem::with_id(MENU_COPY_LAST, "Copy Last Transcript", has_transcript, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id(MENU_QUIT, "Quit Scribe", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &show_item,
            &hide_item,
            &toggle_recording_item,
            &copy_last_item,
            &separator,
            &quit_item,
        ])
        .context("failed to build tray menu")?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Local Transcriber")
            .with_icon(scribe_icon()?)
            .build()
            .context("failed to create tray icon")?;

        Ok(Self {
            _tray_icon: tray_icon,
            toggle_recording_item,
            copy_last_item,
            _show_item: show_item,
            _hide_item: hide_item,
            _quit_item: quit_item,
            _separator: separator,
        })
    }

    pub fn set_recording(&self, is_recording: bool) {
        self.toggle_recording_item
            .set_text(recording_label(is_recording));
    }

    pub fn set_has_transcript(&self, has_transcript: bool) {
        self.copy_last_item.set_enabled(has_transcript);
    }

    pub fn poll_command(&self) -> Option<TrayCommand> {
        let mut command = None;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(menu_command) = command_from_menu_id(event.id().as_ref()) {
                command = Some(menu_command);
            }
        }
        if command.is_some() {
            while TrayIconEvent::receiver().try_recv().is_ok() {}
            return command;
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if tray_event_should_show(&event) {
                command = Some(TrayCommand::Show);
            }
        }
        command
    }
}

fn command_from_menu_id(id: &str) -> Option<TrayCommand> {
    match id {
        MENU_SHOW => Some(TrayCommand::Show),
        MENU_HIDE => Some(TrayCommand::Hide),
        MENU_TOGGLE_RECORDING => Some(TrayCommand::ToggleRecording),
        MENU_COPY_LAST => Some(TrayCommand::CopyLastTranscript),
        MENU_QUIT => Some(TrayCommand::Quit),
        _ => None,
    }
}

fn tray_event_should_show(event: &TrayIconEvent) -> bool {
    match event {
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } => true,
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => true,
        _ => false,
    }
}

fn recording_label(is_recording: bool) -> &'static str {
    if is_recording {
        "Stop Recording"
    } else {
        "Start Recording"
    }
}

fn scribe_icon() -> Result<Icon> {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside = (2..=13).contains(&x) && (2..=13).contains(&y);
            let mic_stem = (7..=8).contains(&x) && (5..=12).contains(&y);
            let mic_head = (5..=10).contains(&x) && (3..=8).contains(&y);
            let base = (4..=11).contains(&x) && y == 13;
            let (r, g, b, a) = if !inside {
                (0, 0, 0, 0)
            } else if mic_head || mic_stem || base {
                (255, 255, 255, 255)
            } else {
                (24, 102, 196, 255)
            };
            rgba.extend([r, g, b, a]);
        }
    }

    Icon::from_rgba(rgba, SIZE, SIZE).context("failed to create tray icon bitmap")
}
