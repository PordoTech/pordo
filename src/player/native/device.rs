//! Resolve the current default to a concrete WASAPI endpoint before activation.
//! CPAL 0.18.2's default-device async activation/notification path can fail with
//! RPC_E_CHANGED_MODE after hotplug (RustAudio/cpal#1402). The playback worker
//! polls the default ID instead; no COM work runs in endpoint notification callbacks.
use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait};

pub(super) fn open_default() -> Result<(cpal::Device, cpal::DeviceId)> {
    let host = cpal::default_host();
    let id = host
        .default_output_device()
        .context("no output device")?
        .id()?;
    let endpoint = host
        .device_by_id(&id)
        .context("default output endpoint is unavailable")?;
    Ok((endpoint, id))
}

pub(super) fn default_id() -> Option<cpal::DeviceId> {
    cpal::default_host().default_output_device()?.id().ok()
}
