//! Start-up failures the user has to be told about.
//!
//! # The bug this module exists to fix
//!
//! `resumed` used to be a chain of `.expect("gpu context")`. The release
//! profile builds with `panic = "abort"`, so a machine with no usable GPU
//! adapter — a stale driver, a remote desktop session, a VM without 3D — did
//! not report anything at all: the process died, the window never appeared,
//! and the message went to a stderr nobody was reading because the app was
//! launched from a desktop icon.
//!
//! Every one of those failures is now a [`ShellError`] with a title, an
//! explanation, and something to try. The shell shows it in a native dialog and
//! exits cleanly.

/// A failure that stops the window from coming up.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("the windowing system could not be reached: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    #[error("the window could not be created: {0}")]
    Window(#[from] winit::error::OsError),
    #[error("a drawing surface could not be created for the window: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    #[error("no usable graphics adapter: {0}")]
    Gpu(#[source] anyhow::Error),
    #[error(
        "this graphics adapter offers no 8-bit surface format the canvas can draw to \
         (it reported: {formats})"
    )]
    UnsupportedSurfaceFormat { formats: String },
}

impl ShellError {
    /// The dialog's title bar.
    pub fn title(&self) -> &'static str {
        match self {
            ShellError::EventLoop(_) | ShellError::Window(_) => {
                "Raster Studio cannot open a window"
            }
            ShellError::Surface(_)
            | ShellError::Gpu(_)
            | ShellError::UnsupportedSurfaceFormat { .. } => {
                "Raster Studio cannot start the graphics system"
            }
        }
    }

    /// What went wrong, and what the user can do about it.
    ///
    /// Every variant ends with a suggestion. "Graphics initialisation failed"
    /// on its own tells the user only that they cannot use the program.
    pub fn user_message(&self) -> String {
        let advice = match self {
            ShellError::EventLoop(_) => {
                "Raster Studio needs a desktop session. If you are connected over SSH or \
                 running in a container, start it on the machine's own display."
            }
            ShellError::Window(_) => {
                "The desktop refused to create a window. Signing out and back in usually \
                 clears this."
            }
            ShellError::Surface(_) | ShellError::Gpu(_) => {
                "Update your graphics driver and try again. In a virtual machine or a \
                 remote desktop session, enable 3D acceleration for the guest."
            }
            ShellError::UnsupportedSurfaceFormat { .. } => {
                "This usually means a software or remote-display adapter is in use. Update \
                 your graphics driver, or run on the machine's own display."
            }
        };
        format!("{self}\n\n{advice}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<ShellError> {
        vec![
            ShellError::Gpu(anyhow::anyhow!("no suitable GPU adapter found")),
            ShellError::UnsupportedSurfaceFormat {
                formats: "Rgba16Float".into(),
            },
        ]
    }

    #[test]
    fn every_failure_explains_itself_and_suggests_something() {
        for err in samples() {
            let title = err.title();
            assert!(!title.is_empty());
            let message = err.user_message();
            assert!(
                message.lines().count() >= 3,
                "a bare one-liner is not an explanation: {message}"
            );
            assert!(
                message.contains(&err.to_string()),
                "the message must contain what actually failed"
            );
            assert!(
                message.contains("driver")
                    || message.contains("display")
                    || message.contains("session"),
                "no advice in: {message}"
            );
        }
    }

    #[test]
    fn the_adapter_failure_names_the_thing_that_failed() {
        let err = ShellError::Gpu(anyhow::anyhow!("no suitable GPU adapter found"));
        assert!(err.to_string().contains("no suitable GPU adapter found"));
        assert_eq!(
            err.title(),
            "Raster Studio cannot start the graphics system"
        );
    }

    #[test]
    fn an_unusable_surface_format_lists_what_was_offered() {
        let err = ShellError::UnsupportedSurfaceFormat {
            formats: "Rgba16Float, Rgb10a2Unorm".into(),
        };
        assert!(err.to_string().contains("Rgb10a2Unorm"), "{err}");
    }
}
