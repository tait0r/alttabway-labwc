use crate::geometry_provider::GeometryProvider;
use crate::{
    geometry_ipc::{HyprlandIpc, SwayIpc},
    geometry_provider::Geometry,
};
use anyhow::{Result, bail};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

enum GeometryWorkerRequestEvent<U: Copy + Send + 'static> {
    ActiveWindow(U),
}

#[derive(Debug)]
pub enum GeometryWorkerEvent<U: Copy + Send + 'static> {
    ActiveWindow(U, Geometry),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompositorBackend {
    Region,
    ToplevelCapture,
}

#[derive(Debug)]
pub struct GeometryWorker<U: Copy + Send + 'static> {
    request_tx: UnboundedSender<GeometryWorkerRequestEvent<U>>,
    response_rx: UnboundedReceiver<GeometryWorkerEvent<U>>,
    backend: CompositorBackend,
}

impl<U: Copy + Send + 'static> GeometryWorker<U> {
    pub fn new() -> Result<Self> {
        // Hyprland, then Sway. Otherwise the Wayland client selects per-window
        // capture only if the compositor advertises the required globals.
        let mut provider: Option<Box<dyn GeometryProvider + Send>> =
            if let Ok(ipc) = HyprlandIpc::new() {
                Some(Box::new(ipc))
            } else if let Ok(ipc) = SwayIpc::new() {
                Some(Box::new(ipc))
            } else {
                None
            };
        let backend = if provider.is_some() {
            CompositorBackend::Region
        } else {
            CompositorBackend::ToplevelCapture
        };

        let (request_tx, mut request_rx) = mpsc::unbounded_channel();
        let (response_tx, response_rx) = mpsc::unbounded_channel();

        tokio::spawn(async move {
            while let Some(event) = request_rx.recv().await {
                match event {
                    GeometryWorkerRequestEvent::ActiveWindow(user_data) => {
                        let Some(provider) = provider.as_mut() else {
                            continue;
                        };
                        if let Ok(geometry) = provider.get_active_window_geometry()
                            && response_tx
                                .send(GeometryWorkerEvent::ActiveWindow(user_data, geometry))
                                .is_err()
                        {
                            return;
                        }
                    }
                }
            }
        });

        Ok(Self {
            request_tx,
            response_rx,
            backend,
        })
    }

    pub fn backend(&self) -> CompositorBackend {
        self.backend
    }

    pub fn request_active_window_geometry(&mut self, user_data: U) -> Result<()> {
        let result = self
            .request_tx
            .send(GeometryWorkerRequestEvent::ActiveWindow(user_data));

        if result.is_err() {
            bail!("failed to send. geometry worker is down.")
        }

        Ok(())
    }

    pub async fn recv(&mut self) -> Option<GeometryWorkerEvent<U>> {
        self.response_rx.recv().await
    }
}
