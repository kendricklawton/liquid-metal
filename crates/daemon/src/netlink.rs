//! TAP device creation + bridge attachment. Linux only.
#![cfg(target_os = "linux")]

use anyhow::{Context, Result, bail};
use futures::TryStreamExt;
use rtnetlink::new_connection;
use std::ffi::CString;
use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;

const TUNSETIFF: libc::c_ulong = 0x400454ca;
const TUNSETPERSIST: libc::c_ulong = 0x400454cb;
const IFF_TAP: libc::c_short = 0x0002;
const IFF_NO_PI: libc::c_short = 0x1000;

pub fn create_tap(name: &str) -> Result<()> {
    if name.len() >= libc::IFNAMSIZ {
        bail!("tap name too long: {}", name);
    }
    let tun = OpenOptions::new().read(true).write(true).open("/dev/net/tun")
        .context("opening /dev/net/tun")?;
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let name_c = CString::new(name).context("tap name contains NUL")?;
    let bytes = name_c.as_bytes_with_nul();
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ifr.ifr_name.as_mut_ptr() as *mut u8, bytes.len());
        ifr.ifr_ifru.ifru_flags = IFF_TAP | IFF_NO_PI;
        if libc::ioctl(tun.as_raw_fd(), TUNSETIFF, &ifr as *const _) < 0 {
            bail!("TUNSETIFF failed: {}", std::io::Error::last_os_error());
        }
        if libc::ioctl(tun.as_raw_fd(), TUNSETPERSIST, 1i32) < 0 {
            bail!("TUNSETPERSIST failed: {}", std::io::Error::last_os_error());
        }
    }
    tracing::info!(name, "TAP created");
    Ok(())
}

pub async fn attach_to_bridge(tap_name: &str, bridge: &str) -> Result<()> {
    let (conn, handle, _) = new_connection().context("rtnetlink open")?;
    let conn_handle = tokio::spawn(conn);
    let tap_idx = link_index(&handle, tap_name).await?;
    handle.link().set(tap_idx).up().execute().await.context("TAP up")?;
    let br_idx = link_index(&handle, bridge).await?;
    handle.link().set(tap_idx).controller(br_idx).execute().await.context("TAP → bridge")?;
    conn_handle.abort();
    tracing::info!(tap_name, bridge, "TAP attached");
    Ok(())
}

pub async fn delete_tap(tap_name: &str) -> Result<()> {
    let (conn, handle, _) = new_connection().context("rtnetlink open")?;
    let conn_handle = tokio::spawn(conn);
    let idx = link_index(&handle, tap_name).await?;
    handle.link().del(idx).execute().await.context("deleting TAP")?;
    conn_handle.abort();
    tracing::info!(tap_name, "TAP deleted");
    Ok(())
}

async fn link_index(handle: &rtnetlink::Handle, name: &str) -> Result<u32> {
    let link = handle.link().get().match_name(name.to_string()).execute()
        .try_next().await.context("rtnetlink get")?
        .with_context(|| format!("link '{}' not found", name))?;
    Ok(link.header.index)
}
