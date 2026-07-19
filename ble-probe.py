# /// script
# requires-python = ">=3.10"
# dependencies = ["bleak>=0.22"]
# ///
"""
BLE reverse-engineering harness for Attack Shark X3 / Kysona M600 (M600-5.2).

Usage:
  uv run ble-probe.py --help
  uv run ble-probe.py --list
  uv run ble-probe.py --self-test
  uv run ble-probe.py --dry-run prefs-checksum-ab
  uv run ble-probe.py prefs-checksum-ab
  uv run ble-probe.py --danger dpi-write
  uv run ble-probe.py --read-fee1
  uv run ble-probe.py --danger --cmd 050f011003a8636363010401e9
  uv run ble-probe.py --output results.jsonl prefs-checksum-ab

Safety: no writes occur without explicit experiment selection.
The prefs-checksum-ab experiment is LOW RISK (gated but not --danger).
All other experiments and --cmd require --danger (except in --dry-run).

Limitation: ACK correlation is by report ID only (no transaction ID).
If two same-RID packets are sent back-to-back and the first ACK is delayed,
the second write may mis-correlate the stale ACK. To mitigate:
 - Stale ACKs are drained before every write.
 - Within an experiment, a write error or ACK timeout aborts all remaining
   steps (even without --fail-fast).
 - After an ACK mismatch, an extra delay + drain runs before the next step.
This cannot provide transaction-perfect correlation without a
sequence-numbered protocol.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
import time
from dataclasses import dataclass, field
from typing import Any, Callable, Optional, Tuple

# ── constants ──────────────────────────────────────────────────────

TARGET_NAME_DEFAULT = "M600-5.2"
FEE0_UUID = "0000fee0-0000-1000-8000-00805f9b34fb"
FEE1_UUID = "0000fee1-0000-1000-8000-00805f9b34fb"
ACK_TIMEOUT_DEFAULT = 2.0   # seconds to wait for FEE4 ACK per write
CONNECT_TIMEOUT_DEFAULT = 10.0  # seconds for BleakClient connection
SCAN_DURATION = 8  # seconds to scan for device

# ── ACK parser ─────────────────────────────────────────────────────

@dataclass
class Ack:
    """Parsed FEE4 notification: 10 50 <status> <report_id> (4 bytes)."""
    raw: bytes
    status: int
    report_id: int

    @classmethod
    def parse(cls, data: bytes) -> Optional[Ack]:
        if len(data) >= 4 and data[0] == 0x10 and data[1] == 0x50:
            return cls(raw=bytes(data[:4]), status=data[2], report_id=data[3])
        return None


# ── checksum helpers ───────────────────────────────────────────────

def _checksum_x11_8bit(payload: bytes) -> int:
    """X11 legacy: 8-bit sum of payload (including state byte at end), mod 256."""
    return sum(payload) & 0xFF


def _checksum_x3_16bit(payload: bytes) -> int:
    """X3: 16-bit big-endian sum of 8-byte data payload (bytes 3-10)."""
    return sum(payload) & 0xFFFF


def _explain_checksum_ab() -> list[str]:
    """Return human-readable explanation lines for the two checksum packets."""
    pkt_legacy = bytes.fromhex("050f011003a8636363010400e9")
    pkt_x3 = bytes.fromhex("050f011003a8636363010401e9")

    data = pkt_legacy[3:11]  # bytes 3-10: 10 03 a8 63 63 63 01 04
    data_sum = sum(data)  # 0x01E9 = 489

    payload_x11 = pkt_legacy[3:12]  # includes byte 11 = 0x00
    cs_x11 = _checksum_x11_8bit(payload_x11)
    expected_cs_x11 = pkt_legacy[12]

    cs_x3 = _checksum_x3_16bit(data)
    expected_cs_x3 = (pkt_x3[11] << 8) | pkt_x3[12]

    lines = [
        "── prefs-checksum-ab: checksum explanation ──",
        "",
        f"  Shared data (bytes 3-10): {data.hex(' ')}",
        f"  Sum of data bytes: 0x{data_sum:04X} ({data_sum})",
        "",
        "  Step 1 — legacy X11 8-bit checksum:",
        f"    Payload (bytes 3-11):  {payload_x11.hex(' ')}  (includes state flag 0x00)",
        f"    Checksum = sum(payload) & 0xFF = 0x{cs_x11:02X}",
        f"    Stored at byte 12: 0x{expected_cs_x11:02X}  {'✓ match' if cs_x11 == expected_cs_x11 else '✗ MISMATCH'}",
        "    Expected ACK: status=0x01 (rejected by X3 firmware)",
        "",
        "  Step 2 — X3 16-bit big-endian checksum:",
        f"    Payload (bytes 3-10):  {data.hex(' ')}",
        f"    Checksum = sum(payload) = 0x{cs_x3:04X}",
        f"    Stored big-endian at bytes 11-12: 0x{expected_cs_x3:04X}  {'✓ match' if cs_x3 == expected_cs_x3 else '✗ MISMATCH'}",
        "    Expected ACK: status=0x00 (accepted by X3 firmware)",
        "",
        "  Hypothesis: X3 firmware expects 16-bit checksum. The legacy X11",
        "  8-bit checksum with state flag at byte 11 is rejected. If both are",
        "  accepted, firmware may be tolerant. If both rejected, firmware may",
        "  use a different checksum scheme entirely.",
    ]
    return lines


def _explain_button_checksum_ab() -> list[str]:
    """Return concise explanation for the button-map checksum A/B packets."""
    pkt_legacy = bytes.fromhex("083b010200000300000400000d00003c00000f00001100730500003c00000100000100000100000100000100000100000100000a00000900000040")
    pkt_x3 = bytes.fromhex("083b010200000300000400000d00003c00000f00001100730500003c00000100000100000100000100000100000100000100000a00000900000140")

    data = pkt_legacy[2:57]  # 55 bytes
    data_sum = sum(data)  # 0x0141
    cs = (data_sum - 1) & 0xFFFF  # 0x0140

    stored_legacy = (pkt_legacy[57] << 8) | pkt_legacy[58]
    stored_x3 = (pkt_x3[57] << 8) | pkt_x3[58]

    lines = [
        "── button-checksum-ab: checksum explanation ──",
        "",
        f"  Report 0x08, 59 bytes. Data range bytes 2..56 (55 bytes):",
        f"    sum = 0x{data_sum:04X} ({data_sum})",
        f"    checksum = (sum - 1) & 0xFFFF = 0x{cs:04X}",
        "",
        "  Step 1 — legacy X11 8-bit checksum:",
        f"    Bytes 57..58: 0x{stored_legacy:04X}  (state flag 0x00 + low byte 0x40)",
        "    Expected ACK: status=0x01 (rejected by X3 firmware)",
        "",
        "  Step 2 — X3 16-bit big-endian checksum:",
        f"    Bytes 57..58: 0x{stored_x3:04X}  {'✓ match' if stored_x3 == cs else '✗ MISMATCH'}",
        "    Expected ACK: status=0x00 (accepted by X3 firmware)",
        "",
        "  Temporary mapping: Forward button → F24 (no modifiers)",
        f"    Bytes 21..23: 11 00 73  (FirmwareAction.KEYBOARD, mods=0, HID F24)",
        "    USB factory reset restores stock mapping.",
        "",
        "  Hypothesis: same X11 vs X3 checksum format mismatch as 0x05 prefs.",
    ]
    return lines


# ── experiment registry ────────────────────────────────────────────

@dataclass
class ExperimentStep:
    packet: bytes
    label: str
    # None means: an ACK is still required (timeout = error) but any status
    # value is accepted for pass/fail classification.  Use None when the
    # firmware behaviour is unpredictable (e.g. may crash or accept).
    expected_ack_status: Optional[int]


@dataclass
class Experiment:
    name: str
    description: str
    dangerous: bool
    steps: list[ExperimentStep]


# ── preserved packet builders ──────────────────────────────────────

def _mk_dpi() -> bytes:
    """X3 Wired DPI packet: stages 400/900/1800/3600/7200/14400, active stage 1."""
    p = bytearray(52)
    p[0] = 0x04; p[1] = 0x38; p[2] = 0x01
    p[3] = 0x00; p[4] = 0x00; p[5] = 0x3f; p[6] = 0x00; p[7] = 0x00
    for i, raw in enumerate([7, 0x11, 0x23, 0x47, 0x8f, 0x1f]):
        p[8 + i] = raw
    p[14] = p[15] = p[22] = p[23] = 0x00
    p[24] = 0x01  # active stage 1
    x3f = [0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00,
           0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00,
           0xff, 0xff, 0x40, 0x00, 0xff, 0xff, 0xff, 0x01]
    for i, b in enumerate(x3f):
        p[25 + i] = b
    cs = sum(p[3:50]) & 0xffff
    p[50] = (cs >> 8) & 0xff
    p[51] = cs & 0xff
    return bytes(p)


def _mk_polling(hz: int = 1000, wired: bool = True) -> bytes:
    """Polling rate packet. hz=125|250|500|1000."""
    rate = {125: 0x08, 250: 0x04, 500: 0x02, 1000: 0x01}[hz]
    p = bytearray(9)
    p[0] = 0x06; p[1] = rate
    if wired:
        p[8] = (0xff - rate) & 0xff
    return bytes(p)


def _mk_prefs_default() -> bytes:
    """Known-good prefs (0x05) for X3 wired — LED off (mode 0x00), all RGB 0."""
    mode = 0x00               # LightMode.Off — known to crash firmware over BLE
    bucket_speed = 0x05       # bucket 0 << 4 | hardwareSpeed(led=1 => 6-1=5)
    deep_sleep = 0xA8         # 10 minutes = 0x08 + 10*0x10
    r, g, b = 0x00, 0x00, 0x00
    sleep_raw = 0x0A          # 5 minutes * 2
    debounce = 0x02           # 4ms = (4-4)/2 + 2
    dynamic = 0x01
    payload = bytes([mode, bucket_speed, deep_sleep, r, g, b, sleep_raw, debounce])
    cs = sum(payload) & 0xff
    return bytes([0x05, 0x0f, 0x01]) + payload + bytes([dynamic, cs])


# ── experiment registry ────────────────────────────────────────────

LEGACY_PKT = bytes.fromhex("050f011003a8636363010400e9")
X3_PKT = bytes.fromhex("050f011003a8636363010401e9")

BUTTON_LEGACY_PKT = bytes.fromhex(
    "083b010200000300000400000d00003c00000f00001100730500003c00"
    "000100000100000100000100000100000100000100000a00000900000040"
)
BUTTON_X3_PKT = bytes.fromhex(
    "083b010200000300000400000d00003c00000f00001100730500003c00"
    "000100000100000100000100000100000100000100000a00000900000140"
)

EXPERIMENTS: dict[str, Experiment] = {
    "prefs-checksum-ab": Experiment(
        name="prefs-checksum-ab",
        description=(
            "Write prefs (0x05) with LED mode 0x10 using two checksum formats: "
            "X11 8-bit legacy (expect reject 0x01) then X3 16-bit (expect accept 0x00). "
            "LOW RISK — uses LED mode 0x10 (>= 0x10 avoids firmware crash), "
            "but writes preference bytes (deep sleep, RGB, debounce, sleep timer). "
            "May change settings."
        ),
        dangerous=False,
        steps=[
            ExperimentStep(packet=LEGACY_PKT, label="legacy/8-bit checksum", expected_ack_status=0x01),
            ExperimentStep(packet=X3_PKT, label="X3/16-bit checksum", expected_ack_status=0x00),
        ],
    ),
    "dpi-write": Experiment(
        name="dpi-write",
        description="Write DPI stages (0x04, 52 bytes). Changes mouse DPI settings.",
        dangerous=True,
        steps=[ExperimentStep(packet=_mk_dpi(), label="DPI write", expected_ack_status=0x00)],
    ),
    "polling-write": Experiment(
        name="polling-write",
        description=(
            "Write polling rate 1000 Hz (0x06). Known to be rejected (0x01) over BLE. "
            "Stock app explicitly skips BLE for report 0x06."
        ),
        dangerous=True,
        steps=[ExperimentStep(packet=_mk_polling(1000), label="polling 1000Hz", expected_ack_status=0x01)],
    ),
    "prefs-led-off": Experiment(
        name="prefs-led-off",
        description=(
            "Write prefs (0x05) with LED mode 0x00 (LightMode.Off). "
            "**Known to crash firmware over BLE.** Requires --danger. "
            "If firmware crashes, ACK will be missing (timeout=error). "
            "If firmware accepts, ACK status 0x00 is expected but any "
            "status is accepted for pass/fail."
        ),
        dangerous=True,
        steps=[ExperimentStep(packet=_mk_prefs_default(), label="prefs LED off", expected_ack_status=None)],
    ),
    "button-checksum-ab": Experiment(
        name="button-checksum-ab",
        description=(
            "Write button map (0x08, 59 bytes) with Forward→F24 using two checksum "
            "formats: X11 8-bit legacy (expect reject 0x01) then X3 16-bit (expect "
            "accept 0x00). Temporarily remaps Forward to F24 (no modifiers). "
            "USB factory reset restores stock mapping. Requires --danger."
        ),
        dangerous=True,
        steps=[
            ExperimentStep(packet=BUTTON_LEGACY_PKT, label="legacy/8-bit checksum", expected_ack_status=0x01),
            ExperimentStep(packet=BUTTON_X3_PKT, label="X3/16-bit checksum", expected_ack_status=0x00),
        ],
    ),
}


# ── FEE1 counter helpers ───────────────────────────────────────────

def _fee1_counter_delta(before: Optional[bytes], after: Optional[bytes]) -> str:
    """Conservative byte-level delta description for FEE1.

    Known: byte 0 and byte 9 increment in lockstep on every FEE3 write.
    We record raw bytes and describe changes rather than assuming a uint16.
    """
    if before is None or after is None:
        return "N/A (missing snapshot)"
    if len(before) < 10 or len(after) < 10:
        return f"short: before={before.hex()} after={after.hex()}"
    diffs: list[str] = []
    for i in range(min(len(before), len(after))):
        if before[i] != after[i]:
            diffs.append(f"b[{i}]:{before[i]:02x}→{after[i]:02x}")
    if not diffs:
        return "unchanged"
    return "; ".join(diffs)


# ── ACK correlator ─────────────────────────────────────────────────

class AckCorrelator:
    """Correlates FEE4 notifications to writes via an asyncio.Queue by report ID.

    Limitation: correlation is by report ID only.  If two same-RID packets are
    sent before the first ACK arrives, the second write may pick up the first
    ACK.  Callers must guard against this (drain before write, abort same-RID
    experiments on error).
    """

    def __init__(self, ack_timeout: float):
        self._queue: asyncio.Queue[Ack] = asyncio.Queue()
        self.ack_timeout = ack_timeout
        self._ignored: list[Ack] = []

    def on_notify(self, _char_handle: int, data: bytearray) -> None:
        """Bleak notification callback. Push parsed ACKs to queue."""
        ack = Ack.parse(bytes(data))
        if ack is not None:
            self._queue.put_nowait(ack)

    async def drain_stale(self) -> None:
        """Clear any previously queued ACKs before a new write."""
        while not self._queue.empty():
            try:
                self._queue.get_nowait()
            except asyncio.QueueEmpty:
                break

    async def wait_for_ack(self, report_id: int) -> Optional[Ack]:
        """Wait for an ACK matching report_id. Returns None on timeout.

        Unrelated ACKs are logged and ignored while waiting.
        """
        deadline = time.monotonic() + self.ack_timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            try:
                ack = await asyncio.wait_for(self._queue.get(), timeout=remaining)
            except asyncio.TimeoutError:
                return None
            if ack.report_id == report_id:
                return ack
            # Unrelated — log and ignore, keep waiting
            self._ignored.append(ack)
            print(f"  [ignored] ACK report=0x{ack.report_id:02x} status=0x{ack.status:02x} "
                  f"(waiting for report 0x{report_id:02x})")

    def flush_ignored(self) -> list[Ack]:
        """Return and clear the list of ignored ACKs."""
        result = self._ignored[:]
        self._ignored.clear()
        return result


# ── per-step record ────────────────────────────────────────────────

@dataclass
class StepRecord:
    timestamp_iso: str
    timestamp_unix: float
    device: str
    experiment: str
    step: str
    packet_hex: str
    expected_ack_status: Optional[int]
    ack_raw: Optional[str]
    ack_status: Optional[int]
    ack_report_id: Optional[int]
    fee1_before_hex: Optional[str]
    fee1_after_hex: Optional[str]
    counter_delta: Optional[str]
    duration_ms: Optional[float]
    result: str  # "pass", "fail", "error"
    error: Optional[str]

    def to_dict(self) -> dict[str, Any]:
        return {
            "ts": self.timestamp_iso,
            "ts_unix": self.timestamp_unix,
            "device": self.device,
            "experiment": self.experiment,
            "step": self.step,
            "packet": self.packet_hex,
            "expected_ack": self.expected_ack_status,
            "ack_raw": self.ack_raw,
            "ack_status": self.ack_status,
            "ack_report_id": self.ack_report_id,
            "fee1_before": self.fee1_before_hex,
            "fee1_after": self.fee1_after_hex,
            "counter_delta": self.counter_delta,
            "duration_ms": self.duration_ms,
            "result": self.result,
            "error": self.error,
        }


# ── BLE probe state ────────────────────────────────────────────────

@dataclass
class ProbeState:
    client: Any  # BleakClient, imported lazily for --self-test compatibility
    fee3_handle: int = 0
    fee4_char: Any = None
    fee4_ok: bool = False   # FEE4 found and notify subscribed
    fee5_char: Any = None
    ffc1_char: Any = None
    ffc2_char: Any = None
    correlator: AckCorrelator = field(default_factory=lambda: AckCorrelator(ACK_TIMEOUT_DEFAULT))
    fee1_snapshots: list[tuple[str, bytes]] = field(default_factory=list)


# ── BLE operations ─────────────────────────────────────────────────

async def _map_characteristics(client: Any, st: ProbeState) -> None:
    for svc in client.services:
        for ch in svc.characteristics:
            s = str(ch.uuid)
            if "fee3" in s:
                st.fee3_handle = ch.handle
            elif "fee4" in s:
                st.fee4_char = ch
            elif "fee5" in s:
                st.fee5_char = ch
            elif "ffc1" in s:
                st.ffc1_char = ch
            elif "ffc2" in s:
                st.ffc2_char = ch

    if st.fee4_char and "notify" in st.fee4_char.properties:
        await client.start_notify(st.fee4_char, st.correlator.on_notify)
        st.fee4_ok = True
        print("  subscribed FEE4 (ACK notifications)")
    else:
        print("  WARNING: FEE4 notify not available — ACK correlation impossible")


async def _read_fee1(client: Any, label: str, st: ProbeState) -> Optional[bytes]:
    try:
        v = await client.read_gatt_char(FEE1_UUID)
        raw = bytes(v)
        st.fee1_snapshots.append((label, raw))
        print(f"  [{label}] FEE1: {raw.hex(' ')}  ({len(raw)}b)")
        return raw
    except Exception as e:
        print(f"  [{label}] FEE1 read ERROR: {e}")
        return None


async def _write_one(
    client: Any,
    st: ProbeState,
    step: ExperimentStep,
    delay: float,
    output_file: Optional[str],
    device_name: str,
    experiment_name: str,
) -> StepRecord:
    """Write a single packet, wait for correlated ACK, return a StepRecord."""
    report_id = step.packet[0]
    ack_timeout = st.correlator.ack_timeout
    now_unix = time.time()
    now_iso = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(now_unix))
    ts = time.strftime("%H:%M:%S", time.localtime(now_unix))

    print(f"\n  [{ts}] WRITE {step.label}  report=0x{report_id:02x}  "
          f"({len(step.packet)}b)  {step.packet.hex(' ')}")

    # Read FEE1 before
    fee1_before = await _read_fee1(client, f"before-{step.label}", st)

    # Drain stale ACKs, then write
    await st.correlator.drain_stale()

    t_start = time.monotonic()
    write_error: Optional[str] = None
    try:
        await client.write_gatt_char(st.fee3_handle, step.packet, response=True)
    except Exception as e:
        write_error = f"{type(e).__name__}: {e}"
        print(f"  [{ts}]   -> WRITE ERROR: {write_error}")

    # Wait for correlated ACK
    ack: Optional[Ack] = None
    if write_error is None:
        ack = await st.correlator.wait_for_ack(report_id)
    t_end = time.monotonic()
    duration_ms = (t_end - t_start) * 1000.0

    # Read FEE1 after (even if write errored)
    await asyncio.sleep(max(0.0, delay - (t_end - t_start)))
    fee1_after = await _read_fee1(client, f"after-{step.label}", st)

    # Determine result
    ack_raw_str: Optional[str] = None
    ack_status: Optional[int] = None
    ack_rid: Optional[int] = None
    result: str
    error: Optional[str] = None

    if write_error:
        result = "error"
        error = write_error
    elif ack is None:
        result = "error"
        error = f"no ACK received (timeout={ack_timeout:.1f}s)"
    else:
        ack_raw_str = ack.raw.hex()
        ack_status = ack.status
        ack_rid = ack.report_id
        if ack.report_id != report_id:
            # Shouldn't happen since we correlated, but be defensive
            result = "fail"
            error = f"ACK report_id mismatch: expected 0x{report_id:02x}, got 0x{ack.report_id:02x}"
        elif step.expected_ack_status is not None and ack.status != step.expected_ack_status:
            result = "fail"
            error = (f"expected ACK status 0x{step.expected_ack_status:02x}, "
                     f"got 0x{ack.status:02x}")
        else:
            result = "pass"

    # Log
    if ack:
        ack_desc = f"ACK raw={ack_raw_str} status=0x{ack_status:02x} report=0x{ack_rid:02x}"
    else:
        ack_desc = f"ACK MISSING (timeout={ack_timeout:.1f}s)"
    print(f"  [{ts}]   -> {result.upper()}: {ack_desc}")
    if error:
        print(f"  [{ts}]   -> detail: {error}")

    record = StepRecord(
        timestamp_iso=now_iso,
        timestamp_unix=now_unix,
        device=device_name,
        experiment=experiment_name,
        step=step.label,
        packet_hex=step.packet.hex(),
        expected_ack_status=step.expected_ack_status,
        ack_raw=ack_raw_str,
        ack_status=ack_status,
        ack_report_id=ack_rid,
        fee1_before_hex=fee1_before.hex() if fee1_before else None,
        fee1_after_hex=fee1_after.hex() if fee1_after else None,
        counter_delta=_fee1_counter_delta(fee1_before, fee1_after),
        duration_ms=round(duration_ms, 1),
        result=result,
        error=error,
    )

    if output_file:
        _append_jsonl(output_file, record)

    return record


def _append_jsonl(path: str, record: StepRecord) -> None:
    """Append one JSON line (flush immediately)."""
    line = json.dumps(record.to_dict(), ensure_ascii=False)
    with open(path, "a", encoding="utf-8", newline="\n") as f:
        f.write(line + "\n")
        f.flush()


# ── connect + run ──────────────────────────────────────────────────

async def _connect_and_run(
    device_name: str,
    address: Optional[str],
    connect_timeout: float,
    ack_timeout: float,
    requires_write: bool,
    run_fn: Callable[[Any, ProbeState], int],
) -> int:
    """Scan for device if no address, connect, run callback, disconnect.

    Returns: 0 on success, 2 on BLE-level error (not found, connect failed,
    missing required characteristic), or the callback's return value.
    """
    from bleak import BleakScanner, BleakClient

    if address is None:
        print(f"Scanning for {device_name} ...")
        found_addr: Optional[str] = None

        def _scan_cb(dev: Any, adv: Any) -> None:
            nonlocal found_addr
            if found_addr is not None:
                return
            # Match against either advertisement local_name or device name; handle None safely.
            adv_name = adv.local_name if adv.local_name is not None else ""
            dev_name = dev.name if dev.name is not None else ""
            if adv_name == device_name or dev_name == device_name:
                found_addr = dev.address

        async with BleakScanner(_scan_cb, scanning_mode="active"):
            await asyncio.sleep(SCAN_DURATION)
        address = found_addr
        if not address:
            print(f"Not found. Is '{device_name}' in BLE pairing mode?")
            return 2

    print(f"Connecting {address} ...")
    try:
        async with BleakClient(address, timeout=connect_timeout) as client:
            st = ProbeState(client=client, correlator=AckCorrelator(ack_timeout))
            await _map_characteristics(client, st)

            if requires_write:
                if not st.fee3_handle:
                    print("ERROR: FEE3 (write characteristic) not found")
                    return 2
                if not st.fee4_ok:
                    print("ERROR: FEE4 (ACK notify) not available — cannot correlate ACKs for writes")
                    return 2

            print()
            await _read_fee1(client, "baseline", st)
            print()

            result = run_fn(client, st)
            if asyncio.iscoroutine(result):
                result = await result

            print()
            await _read_fee1(client, "final", st)
            # async with context manager handles disconnect automatically
            return result
    except Exception as e:
        print(f"ERROR: {type(e).__name__}: {e}")
        return 2


# ── experiment runner ──────────────────────────────────────────────

async def _run_experiments(
    client: Any,
    st: ProbeState,
    experiments: list[Experiment],
    delay: float,
    output_file: Optional[str],
    device_name: str,
    fail_fast: bool,
) -> int:
    """Run a list of experiments. Return count of failed/errored steps.

    Per-experiment abort on error: if a step has a write error or ACK timeout,
    remaining steps in that experiment are skipped (same-RID ACK correlation
    would be ambiguous).  This happens even without global --fail-fast.
    An ACK mismatch (wrong status) is logged as a failure but the experiment
    continues after an extra drain+delay since the ACK was definite.
    """
    failed = 0
    for exp in experiments:
        print(f"\n── {exp.name}: {exp.description}")
        abort_experiment = False
        for i, step in enumerate(exp.steps):
            if abort_experiment:
                print(f"  !! skipping {step.label} (experiment aborted after previous error)")
                failed += 1
                continue

            record = await _write_one(
                client, st, step, delay, output_file, device_name, exp.name,
            )
            if record.result != "pass":
                failed += 1

            # Determine whether to abort remaining steps
            if record.result == "error":
                # Write error or ACK timeout — same-RID correlation unsafe
                remaining = exp.steps[i + 1:]
                same_rid_remaining = any(s.packet[0] == step.packet[0] for s in remaining)
                if same_rid_remaining:
                    print(f"  !! aborting experiment after error on report 0x{step.packet[0]:02x} "
                          f"(delayed ACK would make same-RID correlation ambiguous)")
                else:
                    print(f"  !! aborting experiment after error (device state uncertain)")
                abort_experiment = True

            elif record.result == "fail" and i + 1 < len(exp.steps):
                # Definite ACK but unexpected status — safe to proceed
                next_step = exp.steps[i + 1]
                if next_step.packet[0] == step.packet[0]:
                    print(f"  [note] ACK mismatch on report 0x{step.packet[0]:02x}; "
                          f"extra drain+delay before next same-RID step")
                    await st.correlator.drain_stale()
                    await asyncio.sleep(delay * 2)

            if abort_experiment and fail_fast:
                print(f"\n!! fail-fast: stopping after {step.label}")
                return failed

            await asyncio.sleep(delay)
    return failed


async def _read_fee1_only(client: Any, st: ProbeState) -> int:
    await _read_fee1(client, "fee1", st)
    return 0


# ── dry-run ────────────────────────────────────────────────────────

def _dry_run_selected(experiments: list[Experiment]) -> None:
    """Print packet details, danger classification, expected ACK without BLE."""
    total_steps = sum(len(e.steps) for e in experiments)
    print(f"DRY RUN: {len(experiments)} experiment(s), {total_steps} step(s) — no BLE scan/connect\n")

    for exp in experiments:
        if exp.dangerous:
            danger_tag = "⚠ DANGEROUS"
        elif exp.name == "prefs-checksum-ab":
            danger_tag = "✓ LOW RISK"
        else:
            danger_tag = "✓ LOW RISK"
        print(f"── {exp.name}  [{danger_tag}]")
        print(f"   {exp.description}")
        for i, step in enumerate(exp.steps):
            report_id = step.packet[0]
            exp_ack = f"0x{step.expected_ack_status:02x}" if step.expected_ack_status is not None else "any (ACK required)"
            print(f"   Step {i+1}: {step.label}")
            print(f"     Packet: {step.packet.hex()}  ({len(step.packet)}b)")
            print(f"     Report ID: 0x{report_id:02x}")
            print(f"     Expected ACK status: {exp_ack}")
        print()

    # Special: checksum explanation if prefs-checksum-ab is selected
    if any(e.name == "prefs-checksum-ab" for e in experiments):
        for line in _explain_checksum_ab():
            print(line)
        print()

    # Special: checksum explanation if button-checksum-ab is selected
    if any(e.name == "button-checksum-ab" for e in experiments):
        for line in _explain_button_checksum_ab():
            print(line)
        print()


# ── cmd validation helper ──────────────────────────────────────────

def _validate_cmd_hex(hex_str: str) -> Tuple[Optional[bytes], Optional[str]]:
    """Validate a --cmd hex string.

    Returns (parsed_bytes, None) on success, or (None, error_message) on failure.
    Empty/whitespace-only strings are rejected.
    """
    if not hex_str or not hex_str.strip():
        return None, "empty --cmd value"
    try:
        b = bytes.fromhex(hex_str.strip())
    except ValueError as e:
        return None, f"invalid hex: {e}"
    if len(b) == 0:
        return None, "empty packet (no hex bytes)"
    return b, None


# ── self-test ──────────────────────────────────────────────────────

def _self_test() -> int:
    """Run offline tests: ACK parsing, checksums, dangerous gate, JSON round-trip,
    cmd validation, registry invariants, timeout wiring, exit propagation.

    Returns 0 on success, non-zero on failure.
    """
    import json as _json

    failures: list[str] = []
    passed = 0

    def check(desc: str, condition: bool, detail: str = "") -> None:
        nonlocal passed
        if condition:
            passed += 1
            print(f"  ✓ {desc}")
        else:
            failures.append(f"{desc}: {detail}")
            print(f"  ✗ {desc}  — {detail}")

    print("── ACK parsing ──")

    ack = Ack.parse(bytes([0x10, 0x50, 0x00, 0x05]))
    check("parse valid ACK (status=0x00 report=0x05)",
          ack is not None and ack.status == 0x00 and ack.report_id == 0x05,
          f"got {ack}")

    ack2 = Ack.parse(bytes([0x10, 0x50, 0x01, 0x06]))
    check("parse valid ACK (status=0x01 report=0x06)",
          ack2 is not None and ack2.status == 0x01 and ack2.report_id == 0x06,
          f"got {ack2}")

    ack3 = Ack.parse(bytes([0x10, 0x51, 0x00, 0x05]))
    check("reject invalid ACK (wrong byte 1)",
          ack3 is None, f"got {ack3}")

    ack4 = Ack.parse(bytes([0x10, 0x50]))
    check("reject short data (2 bytes)", ack4 is None, f"got {ack4}")

    ack5 = Ack.parse(bytes([0x19, 0x64]))
    check("reject non-ACK (battery notification)", ack5 is None, f"got {ack5}")

    print("\n── Checksum packets ──")

    legacy = bytes.fromhex("050f011003a8636363010400e9")
    x3pkt = bytes.fromhex("050f011003a8636363010401e9")

    check("legacy packet length is 13", len(legacy) == 13, f"got {len(legacy)}")
    check("X3 packet length is 13", len(x3pkt) == 13, f"got {len(x3pkt)}")
    check("both packets share same bytes 0-10",
          legacy[:11] == x3pkt[:11],
          f"legacy={legacy[:11].hex()} x3={x3pkt[:11].hex()}")

    data = legacy[3:11]
    check("data bytes (3-10) are expected",
          data == bytes([0x10, 0x03, 0xa8, 0x63, 0x63, 0x63, 0x01, 0x04]),
          f"got {data.hex()}")

    payload_x11 = legacy[3:12]
    cs_x11 = _checksum_x11_8bit(payload_x11)
    check(f"X11 8-bit checksum = 0x{cs_x11:02x} (expected 0xe9)",
          cs_x11 == 0xe9, f"got 0x{cs_x11:02x}")
    check("legacy packet byte 12 matches computed 8-bit checksum",
          legacy[12] == cs_x11, f"pkt[12]=0x{legacy[12]:02x}")

    cs_x3 = _checksum_x3_16bit(data)
    check(f"X3 16-bit checksum = 0x{cs_x3:04X} (expected 0x01E9)",
          cs_x3 == 0x01E9, f"got 0x{cs_x3:04X}")
    stored_x3 = (x3pkt[11] << 8) | x3pkt[12]
    check(f"X3 packet bytes 11-12 = 0x{stored_x3:04X} matches computed checksum",
          stored_x3 == cs_x3, f"stored=0x{stored_x3:04X} computed=0x{cs_x3:04X}")

    print("\n── Button checksum packets (0x08) ──")

    blegacy = BUTTON_LEGACY_PKT
    bx3pkt = BUTTON_X3_PKT

    check("button legacy packet length is 59", len(blegacy) == 59, f"got {len(blegacy)}")
    check("button X3 packet length is 59", len(bx3pkt) == 59, f"got {len(bx3pkt)}")
    check("button packets share same bytes 0-56",
          blegacy[:57] == bx3pkt[:57],
          f"differ at {[i for i in range(57) if blegacy[i] != bx3pkt[i]]}")

    # Sole diff at offset 57
    bdiffs = [i for i in range(len(blegacy)) if blegacy[i] != bx3pkt[i]]
    check("button packets differ only at offset 57", bdiffs == [57], f"diffs={bdiffs}")
    check("button legacy[57] is 0x00", blegacy[57] == 0x00, f"got 0x{blegacy[57]:02x}")
    check("button X3[57] is 0x01", bx3pkt[57] == 0x01, f"got 0x{bx3pkt[57]:02x}")
    check("button byte 58 is 0x40 in both", blegacy[58] == 0x40 and bx3pkt[58] == 0x40,
          f"legacy=0x{blegacy[58]:02x} x3=0x{bx3pkt[58]:02x}")

    # Data range bytes 2..56
    bdata = blegacy[2:57]
    bdata_sum = sum(bdata)
    check(f"button data bytes 2..56 sum = 0x{bdata_sum:04X} (expected 0x0141)",
          bdata_sum == 0x0141, f"got 0x{bdata_sum:04X}")
    bcs = (bdata_sum - 1) & 0xFFFF
    check(f"button checksum (sum-1)&0xFFFF = 0x{bcs:04X} (expected 0x0140)",
          bcs == 0x0140, f"got 0x{bcs:04X}")
    bstored_x3 = (bx3pkt[57] << 8) | bx3pkt[58]
    check(f"button X3 stored 16-bit = 0x{bstored_x3:04X} matches computed",
          bstored_x3 == bcs, f"stored=0x{bstored_x3:04X} computed=0x{bcs:04X}")

    # Mapping bytes at offsets 21..23
    check("button mapping[21] = 0x11 (KEYBOARD)", blegacy[21] == 0x11, f"got 0x{blegacy[21]:02x}")
    check("button mapping[22] = 0x00 (no modifiers)", blegacy[22] == 0x00, f"got 0x{blegacy[22]:02x}")
    check("button mapping[23] = 0x73 (HID F24)", blegacy[23] == 0x73, f"got 0x{blegacy[23]:02x}")

    # Experiment entry invariants
    b_ab = EXPERIMENTS["button-checksum-ab"]
    check("button-checksum-ab has exactly 2 steps", len(b_ab.steps) == 2,
          f"got {len(b_ab.steps)}")
    check("button step 1 expected ACK is 0x01", b_ab.steps[0].expected_ack_status == 0x01)
    check("button step 2 expected ACK is 0x00", b_ab.steps[1].expected_ack_status == 0x00)
    check("button-checksum-ab IS dangerous", b_ab.dangerous,
          f"dangerous={b_ab.dangerous}")
    check("button step 1 packet is BUTTON_LEGACY_PKT", b_ab.steps[0].packet == blegacy)
    check("button step 2 packet is BUTTON_X3_PKT", b_ab.steps[1].packet == bx3pkt)

    print("\n── Dangerous gate ──")

    # prefs-checksum-ab must be non-dangerous (LOW RISK); all others dangerous
    for name, exp in EXPERIMENTS.items():
        if name == "prefs-checksum-ab":
            check(f"'{name}' is NOT dangerous", not exp.dangerous,
                  f"dangerous={exp.dangerous}")
        else:
            check(f"'{name}' IS dangerous", exp.dangerous,
                  f"dangerous={exp.dangerous}")

    # prefs-checksum-ab: 2 steps, expected 0x01 then 0x00
    ab = EXPERIMENTS["prefs-checksum-ab"]
    check("prefs-checksum-ab has exactly 2 steps", len(ab.steps) == 2,
          f"got {len(ab.steps)}")
    check("step 1 expected ACK is 0x01", ab.steps[0].expected_ack_status == 0x01)
    check("step 2 expected ACK is 0x00", ab.steps[1].expected_ack_status == 0x00)
    check("step 1 LED mode is 0x10", legacy[3] == 0x10, f"got 0x{legacy[3]:02x}")
    check("step 2 LED mode is 0x10", x3pkt[3] == 0x10, f"got 0x{x3pkt[3]:02x}")

    # prefs-led-off must have expected_ack_status=None
    lo = EXPERIMENTS["prefs-led-off"]
    check("prefs-led-off expected_ack_status is None (ACK required, any status accepted)",
          lo.steps[0].expected_ack_status is None)

    print("\n── Registry invariants (post-removal) ──")

    check("version-query is NOT in registry", "version-query" not in EXPERIMENTS)
    check("profile-get is NOT in registry", "profile-get" not in EXPERIMENTS)
    check("registry has exactly 5 experiments", len(EXPERIMENTS) == 5,
          f"got {len(EXPERIMENTS)}: {list(EXPERIMENTS.keys())}")
    check("all experiments have at least 1 step",
          all(len(e.steps) >= 1 for e in EXPERIMENTS.values()))
    # Every step must have a report ID (packet non-empty)
    for name, exp in EXPERIMENTS.items():
        for s in exp.steps:
            check(f"{name}/{s.label} packet non-empty", len(s.packet) > 0)

    print("\n── Cmd validation ──")

    b_ok, err = _validate_cmd_hex("00")
    check("valid --cmd '00' returns bytes", b_ok is not None and b_ok == bytes([0x00]),
          f"bytes={b_ok} err={err}")
    b_ok2, err2 = _validate_cmd_hex("050f011003a8636363010400e9")
    check("valid --cmd 13-byte hex returns bytes", b_ok2 is not None and len(b_ok2) == 13,
          f"len={len(b_ok2) if b_ok2 else 'None'} err={err2}")

    b_empty, err_empty = _validate_cmd_hex("")
    check("empty --cmd rejected", b_empty is None and err_empty is not None,
          f"bytes={b_empty} err={err_empty}")
    b_ws, err_ws = _validate_cmd_hex("   ")
    check("whitespace-only --cmd rejected", b_ws is None and err_ws is not None,
          f"bytes={b_ws} err={err_ws}")
    b_bad, err_bad = _validate_cmd_hex("ZZ")
    check("invalid hex --cmd rejected", b_bad is None and err_bad is not None,
          f"bytes={b_bad} err={err_bad}")

    print("\n── Timeout wiring ──")

    c1 = AckCorrelator(ack_timeout=1.5)
    check("AckCorrelator stores custom timeout", c1.ack_timeout == 1.5,
          f"got {c1.ack_timeout}")
    c2 = AckCorrelator(ack_timeout=5.0)
    check("AckCorrelator stores different timeout", c2.ack_timeout == 5.0,
          f"got {c2.ack_timeout}")
    check("two correlators have independent timeouts", c1.ack_timeout != c2.ack_timeout)

    print("\n── JSONL record round-trip ──")

    rec = StepRecord(
        timestamp_iso="2025-01-01T00:00:00",
        timestamp_unix=1735689600.0,
        device="M600-5.2",
        experiment="prefs-checksum-ab",
        step="legacy/8-bit checksum",
        packet_hex="050f011003a8636363010400e9",
        expected_ack_status=0x01,
        ack_raw="10500105",
        ack_status=0x01,
        ack_report_id=0x05,
        fee1_before_hex="01000000000000000001",
        fee1_after_hex="02000000000000000002",
        counter_delta="b[0]:01→02; b[9]:01→02",
        duration_ms=45.2,
        result="pass",
        error=None,
    )
    d = rec.to_dict()
    check("record is a dict", isinstance(d, dict))
    json_str = _json.dumps(d, ensure_ascii=False)
    check("record serializes to JSON", isinstance(json_str, str) and len(json_str) > 0)
    roundtrip = _json.loads(json_str)
    check("record round-trips through JSON",
          roundtrip["experiment"] == "prefs-checksum-ab" and roundtrip["result"] == "pass",
          f"got {roundtrip.get('experiment')} / {roundtrip.get('result')}")

    # Edge: error record
    rec_err = StepRecord(
        timestamp_iso="2025-01-01T00:00:01",
        timestamp_unix=1735689601.0,
        device="M600-5.2",
        experiment="prefs-led-off",
        step="prefs LED off",
        packet_hex="050f010003a80000ff010401af",
        expected_ack_status=None,
        ack_raw=None,
        ack_status=None,
        ack_report_id=None,
        fee1_before_hex="01000000000000000001",
        fee1_after_hex="01000000000000000001",
        counter_delta="unchanged",
        duration_ms=2001.5,
        result="error",
        error="no ACK received (timeout=2.0s)",
    )
    d_err = rec_err.to_dict()
    check("error record has result='error'", d_err["result"] == "error")
    check("error record has expected_ack=null", d_err["expected_ack"] is None)
    check("error record has ack_raw=null", d_err["ack_raw"] is None)
    json_err = _json.dumps(d_err, ensure_ascii=False)
    rt_err = _json.loads(json_err)
    check("error record round-trips", rt_err["experiment"] == "prefs-led-off"
          and rt_err["error"] == "no ACK received (timeout=2.0s)")

    print("\n── Counter delta helper ──")

    b1 = bytes([0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01])
    b2 = bytes([0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02])
    delta = _fee1_counter_delta(b1, b2)
    check("counter delta detects byte 0 and 9 changes",
          "b[0]:01→02" in delta and "b[9]:01→02" in delta,
          f"got '{delta}'")

    b3 = bytes([0x00] * 10)
    delta2 = _fee1_counter_delta(b3, b3)
    check("counter delta reports 'unchanged' when equal",
          delta2 == "unchanged", f"got '{delta2}'")

    delta3 = _fee1_counter_delta(None, b1)
    check("counter delta handles missing snapshot",
          "N/A" in delta3, f"got '{delta3}'")

    # ── summary ──
    total = passed + len(failures)
    print(f"\n{'='*50}")
    print(f"Self-test: {passed}/{total} passed")
    if failures:
        print(f"FAILURES ({len(failures)}):")
        for f in failures:
            print(f"  ✗ {f}")
        return 1
    else:
        print("All tests passed.")
        return 0


# ── argument parsing ───────────────────────────────────────────────

def _build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description="BLE reverse-engineering harness for Attack Shark X3 / Kysona M600",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Safety: no writes occur without explicit experiment selection.\n"
            "The prefs-checksum-ab experiment is LOW RISK (no --danger required).\n"
            "All other experiments and --cmd require --danger (except in --dry-run).\n"
            "\n"
            "Limitation: ACK correlation is by report ID only (no transaction ID).\n"
            "Same-RID steps are drained before write; experiments abort on error\n"
            "to avoid mis-correlating stale ACKs."
        ),
    )
    p.add_argument(
        "experiments", nargs="*", metavar="EXPERIMENT",
        help="Named experiment(s) to run (use --list to see available)",
    )
    p.add_argument(
        "--list", action="store_true",
        help="List all registered experiments and exit",
    )
    p.add_argument(
        "--name", default=TARGET_NAME_DEFAULT,
        help=f"BLE device name to scan for (default: {TARGET_NAME_DEFAULT})",
    )
    p.add_argument(
        "--address", default=None,
        help="BLE address (skip scan, connect directly)",
    )
    p.add_argument(
        "--delay", type=float, default=0.3,
        help="Delay in seconds between writes (default: 0.3)",
    )
    p.add_argument(
        "--timeout", type=float, default=ACK_TIMEOUT_DEFAULT,
        help=f"ACK timeout in seconds per write (default: {ACK_TIMEOUT_DEFAULT})",
    )
    p.add_argument(
        "--connect-timeout", type=float, default=CONNECT_TIMEOUT_DEFAULT,
        help=f"BLE connection timeout in seconds (default: {CONNECT_TIMEOUT_DEFAULT})",
    )
    p.add_argument(
        "--output", default=None, metavar="FILE",
        help="Append JSONL results to FILE",
    )
    p.add_argument(
        "--dry-run", action="store_true",
        help="Print experiment details without BLE scan or connection",
    )
    p.add_argument(
        "--danger", action="store_true",
        help="Allow dangerous experiments and --cmd writes",
    )
    p.add_argument(
        "--read-fee1", action="store_true",
        help="Read FEE1 counter and exit (no writes; does not require FEE3/FEE4)",
    )
    p.add_argument(
        "--cmd", default=None, metavar="HEX",
        help="Send raw hex packet to FEE3 (requires --danger or --dry-run)",
    )
    p.add_argument(
        "--fail-fast", action="store_true",
        help="Stop on first experiment step failure (error aborts same experiment regardless)",
    )
    p.add_argument(
        "--self-test", action="store_true",
        help="Run offline self-tests (no BLE hardware required) and exit",
    )
    return p


# ── main ───────────────────────────────────────────────────────────

async def _async_main(args: argparse.Namespace) -> int:
    device_name: str = args.name
    address: Optional[str] = args.address
    delay: float = args.delay
    ack_timeout: float = args.timeout
    connect_timeout: float = args.connect_timeout
    output_file: Optional[str] = args.output
    fail_fast: bool = args.fail_fast

    selected_names: list[str] = args.experiments

    # ── Resolve experiments ──
    selected: list[Experiment] = []
    unknown: list[str] = []
    for name in selected_names:
        if name in EXPERIMENTS:
            selected.append(EXPERIMENTS[name])
        else:
            unknown.append(name)

    if unknown:
        print(f"Unknown experiment(s): {', '.join(unknown)}")
        print("Use --list to see available experiments.")
        return 1

    # ── Build synthetic experiment for --cmd ──
    if args.cmd is not None:
        cmd_bytes, cmd_err = _validate_cmd_hex(args.cmd)
        if cmd_err is not None:
            print(f"ERROR: invalid --cmd: {cmd_err}")
            return 1
        assert cmd_bytes is not None
        cmd_report_id = cmd_bytes[0]
        cmd_exp = Experiment(
            name="--cmd",
            description=f"Raw packet to FEE3: {cmd_bytes.hex()}",
            dangerous=True,
            steps=[ExperimentStep(
                packet=cmd_bytes,
                label=f"raw 0x{cmd_report_id:02x}",
                expected_ack_status=None,
            )],
        )
        # --cmd is dangerous unless --dry-run
        if not args.danger and not args.dry_run:
            print("ERROR: --cmd writes to device. Use --danger to confirm, or --dry-run to preview.")
            return 1
        selected.append(cmd_exp)

    # ── Dry-run (bypasses danger gate — no BLE connection occurs) ──
    if args.dry_run:
        if not selected:
            print("DRY RUN: no experiments or --cmd selected. Nothing to preview.")
            return 0
        _dry_run_selected(selected)
        return 0

    # ── Danger gate (for actual BLE operations) ──
    any_dangerous = any(e.dangerous for e in selected)
    if any_dangerous and not args.danger:
        dangerous_names = [e.name for e in selected if e.dangerous]
        print(f"ERROR: experiment(s) require --danger: {', '.join(dangerous_names)}")
        print("These experiments write to the device and may change settings or crash firmware.")
        print("Use --danger to confirm, or --dry-run to preview without connecting.")
        return 1

    # ── If nothing selected, show help ──
    if not selected and not args.read_fee1:
        print("No experiments selected. Use --list to see available, or --help for usage.")
        print("Example: uv run ble-probe.py --dry-run prefs-checksum-ab")
        return 0

    # ── Run ──
    if args.read_fee1 and not selected:
        # Read-only mode — no FEE3/FEE4 required
        code = await _connect_and_run(
            device_name, address, connect_timeout, ack_timeout,
            requires_write=False, run_fn=_read_fee1_only,
        )
        return code

    if args.read_fee1 and selected:
        print("Note: --read-fee1 ignored when experiments are selected; FEE1 is read per-step anyway.")

    code = await _connect_and_run(
        device_name, address, connect_timeout, ack_timeout,
        requires_write=True,
        run_fn=lambda c, s: _run_experiments(c, s, selected, delay, output_file, device_name, fail_fast),
    )
    return code


def main() -> None:
    parser = _build_parser()
    args = parser.parse_args()

    # ── Self-test (offline, no BLE) ──
    if args.self_test:
        sys.exit(_self_test())

    # ── List ──
    if args.list:
        print("Registered experiments:\n")
        for name, exp in EXPERIMENTS.items():
            if exp.dangerous:
                danger = "⚠ DANGEROUS"
            elif name == "prefs-checksum-ab":
                danger = "✓ LOW RISK"
            else:
                danger = "✓ LOW RISK"
            print(f"  {name:25s}  [{danger}]")
            print(f"    {exp.description}")
            print(f"    {len(exp.steps)} step(s)")
            for s in exp.steps:
                rid = s.packet[0]
                if s.expected_ack_status is not None:
                    exp_ack = f"0x{s.expected_ack_status:02x}"
                else:
                    exp_ack = "any (ACK required)"
                print(f"      - {s.label}: report=0x{rid:02x} expected_ack={exp_ack}")
            print()
        print("Use --dry-run <experiment> to preview packets without connecting.")
        return

    # ── Normal operation (may use BLE) ──
    code = asyncio.run(_async_main(args))
    sys.exit(code)


if __name__ == "__main__":
    main()
