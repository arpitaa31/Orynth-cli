//! Shared semantic colors for the Workspace and Advanced Debugger.

use ratatui::style::Color;

#[derive(Clone, Copy)]
pub(super) struct UiTheme {
    pub brand: Color,
    pub accent: Color,
    pub surface: Color,
    pub surface_selected: Color,
    pub text: Color,
    pub muted: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
}

pub(super) const THEME: UiTheme = UiTheme {
    brand: Color::Rgb(69, 190, 210),
    accent: Color::Rgb(102, 170, 224),
    surface: Color::Rgb(24, 34, 44),
    surface_selected: Color::Rgb(35, 55, 68),
    text: Color::Rgb(232, 238, 242),
    muted: Color::Rgb(132, 148, 160),
    success: Color::Rgb(88, 190, 126),
    warning: Color::Rgb(224, 177, 80),
    danger: Color::Rgb(224, 105, 105),
    info: Color::Rgb(115, 166, 224),
};
