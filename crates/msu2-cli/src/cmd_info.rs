//! 设备信息、串口列表和触摸键读取。

use std::time::Duration;

use anyhow::Context;
use msu2_core::{Msu2Device, SfrValue};

pub(crate) fn cmd_ports() -> anyhow::Result<()> {
    let ports = msu2_core::list_ports().context("failed to enumerate serial ports")?;

    if ports.is_empty() {
        println!("No serial ports found.");
        return Ok(());
    }

    println!(
        "{:<24} {:<11} {:<24} {:<18} MSU2",
        "PORT", "VID:PID", "PRODUCT", "SERIAL"
    );
    for port in ports {
        println!(
            "{:<24} {:<11} {:<24} {:<18} {}",
            port.path,
            port.vid_pid(),
            port.product.as_deref().unwrap_or("-"),
            port.serial_number.as_deref().unwrap_or("-"),
            if port.is_msu2() { "yes" } else { "" }
        );
    }

    Ok(())
}

pub(crate) async fn cmd_info(device: &mut Msu2Device) -> anyhow::Result<()> {
    println!("Port: {}", device.path());
    println!("Firmware: {}", device.version_string());

    if let Ok(ports) = msu2_core::list_ports() {
        if let Some(port) = ports
            .iter()
            .find(|port| port.path.as_str() == device.path())
        {
            println!(
                "USB: {} {}",
                port.vid_pid(),
                port.product.as_deref().unwrap_or("")
            );
        }
    }

    let table = device
        .read_sfr_table()
        .await
        .context("failed to read SFR table")?;
    println!("SFR entries: {}", table.entries.len());

    for entry in &table.entries {
        let value = match device.read_named(&table, &entry.name).await {
            Ok(value) => format_sfr_value(&value),
            Err(error) => format!("<error: {error}>"),
        };
        println!(
            "  {:<14} {:<20} {}",
            entry.name,
            entry.kind.description(),
            value
        );
    }

    Ok(())
}

pub(crate) async fn cmd_key(
    device: &mut Msu2Device,
    count: u32,
    interval_ms: u64,
) -> anyhow::Result<()> {
    let first = device
        .read_adc(9)
        .await
        .context("failed to read ADC channel 9")?;
    let second = device
        .read_adc(9)
        .await
        .context("failed to read ADC channel 9")?;

    let baseline = ((u32::from(first) + u32::from(second)) / 2) as u16;
    let threshold = baseline.saturating_sub(125);
    println!("ADC9 baseline={baseline}, press threshold={threshold}");

    for index in 0..count {
        let value = device.read_adc(9).await?;
        let state = if value < threshold {
            "pressed"
        } else {
            "released"
        };
        println!("[{index:>3}] ADC9={value:>4}  {state}");

        if index + 1 < count {
            tokio::time::sleep(Duration::from_millis(interval_ms)).await;
        }
    }

    Ok(())
}
pub(crate) fn format_sfr_value(value: &SfrValue) -> String {
    match value {
        SfrValue::U16(value) => format!("{value} (0x{value:04X})"),
        SfrValue::Bytes(bytes) => hex_bytes(bytes),
        SfrValue::Text(text) => format!("{text:?}"),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
