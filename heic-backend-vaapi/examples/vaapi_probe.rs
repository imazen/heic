//! Smoke test for the VA-API runtime probe.
//!
//! Builds `VaApiBackend`, queries `is_available()`, and exits 0
//! when libva + a HEVC profile + a working VADisplay are reachable
//! on this host, 1 otherwise. Set `LIBVA_DRIVER_NAME=nvidia
//! NVD_BACKEND=egl` for the nvidia-vaapi-driver path used in WSL2.

fn main() {
    use heic_core::HevcBackend;
    let b = heic_backend_vaapi::VaApiBackend::new();
    let avail = b.is_available();
    eprintln!("VaApiBackend::is_available() = {avail}");
    std::process::exit(if avail { 0 } else { 1 });
}
