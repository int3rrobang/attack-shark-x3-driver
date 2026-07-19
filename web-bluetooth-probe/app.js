'use strict';

/* ===================================================================
 * Web Bluetooth probe for Attack Shark X3 / Kysona M600 (M600-5.2/5.4)
 *
 * Safety: read-only by default.  Connecting performs no FEE3/
 * configuration writes (subscribing notifications may update the
 * standard CCCD descriptor — a standard GATT write).  The write panel
 * requires a dual confirmation gate (checkbox + typing WRITE),
 * hard-blocks known-dangerous packets, and requires FEE4 notify
 * subscription for ACK correlation.
 *
 * This probe never requests or accesses FFC0/FFC1/FFC2.
 * =================================================================== */

/* ── constants ──────────────────────────────────────────────────── */

var FEE0_SERVICE = '0000fee0-0000-1000-8000-00805f9b34fb';
var FEE1_READ    = '0000fee1-0000-1000-8000-00805f9b34fb';
var FEE3_WRITE   = '0000fee3-0000-1000-8000-00805f9b34fb';
var FEE4_NOTIFY  = '0000fee4-0000-1000-8000-00805f9b34fb';

var BATTERY_SERVICE    = '0000180f-0000-1000-8000-00805f9b34fb';
var BATTERY_LEVEL_CHAR = '00002a19-0000-1000-8000-00805f9b34fb';

var ACK_TIMEOUT_DEFAULT = 2.0;   // seconds

var M600_DEVICE_NAME_RE = /^M600-5\.[24]$/;

// Strict hex validation: only 0-9 a-f A-F after whitespace stripping
var HEX_RE = /^[0-9a-fA-F]+$/;

/* ── state ──────────────────────────────────────────────────────── */

var bluetoothDevice = null;       // BluetoothDevice instance
var gattServer = null;            // BluetoothRemoteGATTServer
var fee0Service = null;           // FEE0 GATT service
var fee1Char = null;              // FEE1 read characteristic
var fee3Char = null;              // FEE3 write characteristic
var fee4Char = null;              // FEE4 notify characteristic
var batteryService = null;        // optional battery service
var batteryLevelChar = null;      // optional battery level characteristic

var connected = false;            // GATT connection state
var fee4Subscribed = false;       // FEE4 notify successfully subscribed
var connectInFlight = false;      // connection setup in progress
var writeInFlight = false;        // write sequence (pre-read/write/ACK/post-read) in progress
var acquisitionInFlight = false;   // getDevices()/requestDevice() in progress
var acquisitionGeneration = 0;     // monotonic counter; stale results are discarded

// ACK waiter (only one pending at a time — writes are serialized)
var pendingAckResolver = null;
var pendingAckRid = null;
var pendingAckTimer = null;

/* ── DOM references ─────────────────────────────────────────────── */

var $ = function(sel) { return document.querySelector(sel); };

var statusEl        = $('#status');
var logEl           = $('#log');
var devInfoEl       = $('#device-info');
var btnPermitted    = $('#btn-permitted');
var btnChoose       = $('#btn-choose');
var btnConnect      = $('#btn-connect');
var btnDisconnect   = $('#btn-disconnect');
var btnReadFee1     = $('#btn-read-fee1');
var btnReadBattery  = $('#btn-read-battery');
var fee1ValEl       = $('#fee1-value');
var batteryValEl    = $('#battery-value');
var charStatusEl    = $('#char-status');
var writeGateEl     = $('#write-gate');
var chkAckRisk      = $('#chk-ack-risk');
var txtConfirm      = $('#txt-confirm');
var txtHex          = $('#txt-hex');
var btnSend         = $('#btn-send');
var pktPreviewEl    = $('#pkt-preview');
var ackTimeoutInp   = $('#ack-timeout');
var btnClearLog     = $('#btn-clear-log');
var btnCopyLog      = $('#btn-copy-log');
var connStateEl     = $('#conn-state');

/* ── log ────────────────────────────────────────────────────────── */

function ts() {
  return new Date().toLocaleTimeString();
}

function log(msg, level) {
  var prefix = level === 'warn' ? '⚠ ' : level === 'error' ? '✗ ' : '';
  logEl.textContent += '[' + ts() + '] ' + prefix + msg + '\n';
  logEl.scrollTop = logEl.scrollHeight;
}

/* ── helpers ────────────────────────────────────────────────────── */

function bytesToHex(buffer) {
  if (buffer instanceof DataView) {
    var arr = new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength);
    return Array.from(arr, function(b) { return b.toString(16).padStart(2, '0'); }).join(' ');
  }
  if (buffer instanceof ArrayBuffer) {
    return Array.from(new Uint8Array(buffer), function(b) { return b.toString(16).padStart(2, '0'); }).join(' ');
  }
  if (ArrayBuffer.isView(buffer)) {
    return Array.from(new Uint8Array(buffer.buffer, buffer.byteOffset, buffer.byteLength), function(b) { return b.toString(16).padStart(2, '0'); }).join(' ');
  }
  return '';
}

/**
 * Validate and parse a hex string into bytes.
 * Rejects non-hex characters after whitespace strip via anchored regex.
 * Never allows parseInt NaN/partial pairs to become zero bytes.
 */
function hexToBytes(hex) {
  var stripped = hex.replace(/\s+/g, '');
  if (stripped.length === 0) {
    throw new Error('Hex string is empty after stripping whitespace');
  }
  if (stripped.length % 2 !== 0) {
    throw new Error('Hex string must have even length after stripping whitespace (got ' + stripped.length + ' characters)');
  }
  if (!HEX_RE.test(stripped)) {
    throw new Error('Hex string contains invalid characters (only 0-9 a-f A-F allowed after whitespace)');
  }
  var len = stripped.length / 2;
  var bytes = new Uint8Array(len);
  for (var i = 0; i < stripped.length; i += 2) {
    bytes[i / 2] = parseInt(stripped.substring(i, i + 2), 16);
  }
  return bytes;
}

/* ── ACK parser ─────────────────────────────────────────────────── */

/**
 * Parse a FEE4 notification as an ACK.
 * Valid ACK format: 10 50 <status> <report_id> (minimum 4 bytes).
 * Returns { raw, status, reportId } or null.
 */
function parseAck(data) {
  var arr;
  if (data instanceof DataView) {
    arr = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  } else if (data instanceof ArrayBuffer) {
    arr = new Uint8Array(data);
  } else if (ArrayBuffer.isView(data)) {
    arr = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
  } else {
    return null;
  }
  if (arr.length >= 4 && arr[0] === 0x10 && arr[1] === 0x50) {
    return {
      raw: arr.slice(0, 4),
      status: arr[2],
      reportId: arr[3]
    };
  }
  return null;
}

/* ── ACK waiter ─────────────────────────────────────────────────── */

function clearPendingWaiter() {
  if (pendingAckTimer !== null) {
    clearTimeout(pendingAckTimer);
    pendingAckTimer = null;
  }
  if (pendingAckResolver !== null) {
    pendingAckResolver({ timedOut: true });
    pendingAckResolver = null;
  }
  pendingAckRid = null;
}

function createWaiter(reportId, timeoutSec) {
  clearPendingWaiter();

  pendingAckRid = reportId;
  return new Promise(function(resolve) {
    pendingAckResolver = resolve;
    pendingAckTimer = setTimeout(function() {
      if (pendingAckResolver) {
        pendingAckResolver({ timedOut: true });
        pendingAckResolver = null;
        pendingAckTimer = null;
        pendingAckRid = null;
      }
    }, timeoutSec * 1000);
  });
}

/**
 * Called by the FEE4 notification handler.
 * Resolves the pending waiter if the RID matches; otherwise logs and ignores.
 */
function onFee4Notification(event) {
  var value = event.target.value;
  if (!value) return;

  var rawHex = bytesToHex(value);
  log('← FEE4 notify: ' + rawHex);

  var ack = parseAck(value);
  if (!ack) {
    return;
  }

  var ackHex = bytesToHex(ack.raw);
  log('  ACK: raw=' + ackHex + ' status=0x' + ack.status.toString(16).padStart(2, '0') +
      ' report=0x' + ack.reportId.toString(16).padStart(2, '0'));

  if (pendingAckResolver !== null) {
    if (ack.reportId === pendingAckRid) {
      if (pendingAckTimer !== null) {
        clearTimeout(pendingAckTimer);
        pendingAckTimer = null;
      }
      var resolver = pendingAckResolver;
      pendingAckResolver = null;
      pendingAckRid = null;
      resolver({ ack: ack });
    } else {
      log('  (ignored — waiting for report 0x' + pendingAckRid.toString(16).padStart(2, '0') + ')', 'warn');
    }
  }
}

/* ── device acquisition ─────────────────────────────────────────── */

async function acquirePermittedDevice() {
  if (!navigator.bluetooth.getDevices) {
    log('getDevices() not available in this browser.', 'error');
    return null;
  }

  log('Querying permitted Bluetooth devices…');
  var devices;
  try {
    devices = await navigator.bluetooth.getDevices();
  } catch (err) {
    log('getDevices() error: ' + (err.message || err), 'error');
    return null;
  }

  if (!devices || devices.length === 0) {
    log('No permitted devices found. Use "Choose device" to grant permission first.');
    return null;
  }

  var m600Devices = devices.filter(function(d) {
    return d.name && d.name.startsWith('M600-');
  });

  if (m600Devices.length === 0) {
    log('No M600 devices among ' + devices.length + ' permitted device(s).');
    log('Permitted devices: ' + devices.map(function(d) { return d.name || '(unnamed)'; }).join(', '));
    return null;
  }

  var preferred = null;
  for (var i = 0; i < m600Devices.length; i++) {
    if (M600_DEVICE_NAME_RE.test(m600Devices[i].name)) {
      preferred = m600Devices[i];
      break;
    }
  }
  var chosen = preferred || m600Devices[0];

  log('Using permitted device: ' + chosen.name + (preferred ? ' (preferred match)' : ''));
  return chosen;
}

async function acquireChooserDevice() {
  log('Opening device chooser (look for M600-… devices, may require advertising on selected RF slot)…');

  try {
    var device = await navigator.bluetooth.requestDevice({
      filters: [{ namePrefix: 'M600-' }],
      optionalServices: [FEE0_SERVICE, BATTERY_SERVICE]
    });
    log('Selected: ' + (device.name || '(unnamed)'));
    return device;
  } catch (err) {
    if (err.name === 'NotFoundError' ||
        (err.message && (err.message.indexOf('cancelled') !== -1 || err.message.indexOf('cancel') !== -1))) {
      log('Device chooser cancelled or no device selected.');
    } else {
      log('requestDevice error: ' + (err.message || err), 'error');
    }
    return null;
  }
}

/* ── connection management ──────────────────────────────────────── */

async function doConnect(device) {
  if (!device) return;
  if (connectInFlight) {
    log('Connection already in progress.', 'warn');
    return;
  }
  if (connected) {
    log('Already connected. Disconnect first.');
    return;
  }

  connectInFlight = true;
  updateUIState();

  bluetoothDevice = device;
  devInfoEl.textContent = 'Connecting to ' + (device.name || '(unnamed)') + ' (' + (device.id || '?') + ') …';

  try {
    log('Connecting GATT server…');
    gattServer = await device.gatt.connect();
    log('GATT connected.');

    device.addEventListener('gattserverdisconnected', onDisconnect);

    // ── Open FEE0 service ──
    log('Getting FEE0 primary service…');
    fee0Service = await gattServer.getPrimaryService(FEE0_SERVICE);
    log('FEE0 service found.');

    // ── Resolve FEE1 (read) ──
    try {
      fee1Char = await fee0Service.getCharacteristic(FEE1_READ);
      log('FEE1 (read) characteristic resolved.');
    } catch (err) {
      log('FEE1 characteristic not found: ' + (err.message || err), 'error');
      fee1Char = null;
    }

    // ── Resolve FEE3 (write) ──
    try {
      fee3Char = await fee0Service.getCharacteristic(FEE3_WRITE);
      log('FEE3 (write) characteristic resolved.');
    } catch (err) {
      log('FEE3 characteristic not found: ' + (err.message || err), 'error');
      fee3Char = null;
    }

    // ── Resolve FEE4 (notify) ──
    fee4Subscribed = false;
    try {
      fee4Char = await fee0Service.getCharacteristic(FEE4_NOTIFY);
      if (fee4Char.properties && fee4Char.properties.notify) {
        await fee4Char.startNotifications();
        fee4Char.addEventListener('characteristicvaluechanged', onFee4Notification);
        fee4Subscribed = true;
        log('FEE4 (notify) subscribed — ACK correlation enabled.');
      } else {
        log('FEE4 found but notify property not available — ACK correlation impossible, writes disabled.', 'warn');
      }
    } catch (err) {
      log('FEE4 characteristic error: ' + (err.message || err), 'warn');
      fee4Char = null;
    }

    // ── Optional: battery service ──
    try {
      batteryService = await gattServer.getPrimaryService(BATTERY_SERVICE);
      batteryLevelChar = await batteryService.getCharacteristic(BATTERY_LEVEL_CHAR);
      log('Battery service/characteristic resolved.');
    } catch (err) {
      log('Battery service not available (optional): ' + (err.message || err));
      batteryService = null;
      batteryLevelChar = null;
    }

    // ── Success ──
    connected = true;
    updateUIState();
    devInfoEl.textContent = 'Connected: ' + (device.name || '(unnamed)') +
      '  id=' + (device.id || '?');

    log('Connection complete.');

  } catch (err) {
    log('Connection failed: ' + (err.message || err), 'error');
    await cleanupConnection();
    updateUIState();
    devInfoEl.textContent = 'Connection failed.';
  } finally {
    connectInFlight = false;
    updateUIState();
  }
}

async function doDisconnect() {
  if (writeInFlight) {
    log('Cannot disconnect while write is in flight.', 'warn');
    return;
  }
  if (!connected && !gattServer) return;

  log('Disconnecting…');
  clearPendingWaiter();
  await cleanupConnection();
  connected = false;
  connectInFlight = false;
  acquisitionInFlight = false;
  updateUIState();
  devInfoEl.textContent = 'Disconnected.';
  log('Disconnected.');
}

async function cleanupConnection() {
  // Unsubscribe FEE4
  if (fee4Char) {
    try {
      fee4Char.removeEventListener('characteristicvaluechanged', onFee4Notification);
      await fee4Char.stopNotifications();
    } catch (_) { /* ignore */ }
  }

  // Remove disconnect listener
  if (bluetoothDevice) {
    bluetoothDevice.removeEventListener('gattserverdisconnected', onDisconnect);
  }

  // Disconnect GATT
  if (gattServer && gattServer.connected) {
    try {
      gattServer.disconnect();
    } catch (_) { /* ignore */ }
  }

  // Clear references
  gattServer = null;
  fee0Service = null;
  fee1Char = null;
  fee3Char = null;
  fee4Char = null;
  fee4Subscribed = false;
  batteryService = null;
  batteryLevelChar = null;
}

function onDisconnect() {
  log('Device disconnected (gattserverdisconnected event).');
  clearPendingWaiter();
  writeInFlight = false;
  acquisitionInFlight = false;
  connected = false;
  connectInFlight = false;
  gattServer = null;
  fee0Service = null;
  fee1Char = null;
  fee3Char = null;
  fee4Char = null;
  fee4Subscribed = false;
  batteryService = null;
  batteryLevelChar = null;
  updateUIState();
  devInfoEl.textContent = 'Disconnected (device event).';
}

/* ── read operations ────────────────────────────────────────────── */

async function readFee1() {
  if (!connected) {
    log('Not connected — cannot read FEE1.', 'error');
    return;
  }
  if (!fee1Char) {
    log('FEE1 characteristic not available.', 'error');
    return;
  }
  if (!fee1Char.properties || !fee1Char.properties.read) {
    log('FEE1 characteristic does not support read.', 'error');
    return;
  }

  try {
    var value = await fee1Char.readValue();
    var hex = bytesToHex(value);
    log('← FEE1 read: ' + hex + '  (' + value.byteLength + 'b)');
    fee1ValEl.textContent = hex;
    log('  (opaque; observed to change on reads/connection activity; not write proof)');
  } catch (err) {
    log('FEE1 read error: ' + (err.message || err), 'error');
  }
}

async function readBattery() {
  if (!connected) {
    log('Not connected — cannot read battery.', 'error');
    return;
  }
  if (!batteryLevelChar) {
    log('Battery level characteristic not available.', 'warn');
    return;
  }
  if (!batteryLevelChar.properties || !batteryLevelChar.properties.read) {
    log('Battery level characteristic does not support read.', 'error');
    return;
  }

  try {
    var value = await batteryLevelChar.readValue();
    var percent = value.getUint8(0);
    log('← Battery level: ' + percent + '%');
    batteryValEl.textContent = percent + '%';
  } catch (err) {
    log('Battery read error: ' + (err.message || err), 'error');
  }
}

async function subscribeBatteryNotify() {
  if (!connected) {
    log('Not connected — cannot subscribe battery notify.', 'error');
    return;
  }
  if (!batteryLevelChar) {
    log('Battery level characteristic not available.', 'warn');
    return;
  }

  try {
    if (batteryLevelChar.properties && batteryLevelChar.properties.notify) {
      await batteryLevelChar.startNotifications();
      batteryLevelChar.addEventListener('characteristicvaluechanged', function(event) {
        var v = event.target.value;
        var pct = v.getUint8(0);
        log('← Battery notify: ' + pct + '%');
        batteryValEl.textContent = pct + '%';
      });
      log('Subscribed to battery notifications.');
    } else {
      log('Battery notify property not available.');
    }
  } catch (err) {
    log('Battery notify error: ' + (err.message || err), 'error');
  }
}

/* ── write operations ───────────────────────────────────────────── */

function canWrite() {
  if (!connected) return { ok: false, reason: 'Not connected.' };
  if (!fee3Char) return { ok: false, reason: 'FEE3 write characteristic not available.' };
  if (!fee4Subscribed) return { ok: false, reason: 'FEE4 ACK unavailable — notify not subscribed. Writes disabled.' };
  if (writeInFlight) return { ok: false, reason: 'Write already in flight.' };
  return { ok: true };
}

function checkWriteGate() {
  if (!chkAckRisk.checked) {
    log('Write blocked: acknowledge risk checkbox not checked.', 'error');
    return false;
  }
  if (txtConfirm.value.trim() !== 'WRITE') {
    log('Write blocked: type WRITE in confirmation field.', 'error');
    return false;
  }
  return true;
}

function validatePacket(hexStr) {
  var stripped = hexStr.replace(/\s+/g, '');
  if (stripped.length === 0) {
    return { error: 'Packet is empty.' };
  }
  if (stripped.length % 2 !== 0) {
    return { error: 'Hex string must have even length after stripping whitespace. Got ' + stripped.length + ' characters.' };
  }
  if (!HEX_RE.test(stripped)) {
    return { error: 'Hex string contains invalid characters (only 0-9 a-f A-F allowed after whitespace).' };
  }

  var bytes;
  try {
    bytes = hexToBytes(hexStr);
  } catch (e) {
    return { error: 'Invalid hex: ' + (e.message || e) };
  }

  if (bytes.length === 0) {
    return { error: 'Packet has zero bytes.' };
  }

  var reportId = bytes[0];

  // ── Hard-block dangerous reports ──
  if (reportId === 0x06) {
    return { error: 'Report 0x06 is hard-blocked over BLE. The stock app explicitly skips BLE for report 0x06.' };
  }

  if (reportId === 0x05 && bytes.length > 3 && bytes[3] === 0x00) {
    return {
      error: 'Report 0x05 with byte 3 = 0x00 is hard-blocked: ' +
        'this writes LED mode 0x00 (LightMode.Off), which is known to crash X3 firmware over BLE. ' +
        'Use byte 3 >= 0x10 for safe LED modes.'
    };
  }

  return { bytes: bytes, reportId: reportId };
}

function validateAckTimeout() {
  var val = parseFloat(ackTimeoutInp.value);
  if (isNaN(val) || !isFinite(val)) {
    return { error: 'ACK timeout must be a finite number.' };
  }
  if (val < 0.5 || val > 30) {
    return { error: 'ACK timeout must be between 0.5 and 30 seconds.' };
  }
  return { value: val };
}

async function doWrite() {
  // ── Pre-flight checks ──
  var canW = canWrite();
  if (!canW.ok) {
    log('Write rejected: ' + canW.reason, 'error');
    return;
  }
  if (!checkWriteGate()) return;

  var timeoutResult = validateAckTimeout();
  if (timeoutResult.error) {
    log('ACK timeout error: ' + timeoutResult.error, 'error');
    return;
  }
  var ackTimeout = timeoutResult.value;

  var hexStr = txtHex.value;
  var result = validatePacket(hexStr);
  if (result.error) {
    log('Write validation error: ' + result.error, 'error');
    pktPreviewEl.textContent = 'Error: ' + result.error;
    pktPreviewEl.style.color = '#f66';
    return;
  }

  var bytes = result.bytes;
  var reportId = result.reportId;

  // ── Show preview ──
  pktPreviewEl.textContent = 'Preview: ' + bytesToHex(bytes) +
    '  length=' + bytes.length + '  RID=0x' + reportId.toString(16).padStart(2, '0');
  pktPreviewEl.style.color = '#e0e0e0';

  log('── write: report=0x' + reportId.toString(16).padStart(2, '0') +
    '  ' + bytes.length + 'b  ' + bytesToHex(bytes));

  // ── Enter write-in-flight ──
  writeInFlight = true;
  updateUIState();

  var tStart = performance.now();

  try {
    // ── Read FEE1 before (observational snapshot only, not proof) ──
    if (fee1Char && connected) {
      try {
        var fee1Before = await fee1Char.readValue();
        log('  FEE1 (before): ' + bytesToHex(fee1Before));
      } catch (err) {
        log('  FEE1 (before) read error: ' + (err.message || err), 'warn');
      }
    }

    // ── Create waiter for ACK correlation ──
    var waiterPromise = createWaiter(reportId, ackTimeout);

    // ── Write ──
    var writeError = null;
    try {
      if (typeof fee3Char.writeValueWithResponse === 'function') {
        await fee3Char.writeValueWithResponse(bytes);
      } else {
        log('  writeValueWithResponse not available, falling back to writeValue (no response guarantee)', 'warn');
        await fee3Char.writeValue(bytes);
      }
      log('  → write sent');
    } catch (err) {
      writeError = err;
      log('  → write error: ' + (err.message || err), 'error');
      clearPendingWaiter();
    }

    // ── Wait for ACK ──
    var ackResult = null;
    if (!writeError) {
      ackResult = await waiterPromise;
    }
    var tEnd = performance.now();
    var durationMs = (tEnd - tStart).toFixed(1);

    if (!writeError && ackResult) {
      if (ackResult.timedOut) {
        log('  → ACK timeout after ' + ackTimeout + 's (duration: ' + durationMs + 'ms)', 'error');
      } else if (ackResult.ack) {
        log('  → ACK received: status=0x' + ackResult.ack.status.toString(16).padStart(2, '0') +
          '  report=0x' + ackResult.ack.reportId.toString(16).padStart(2, '0') +
          '  duration=' + durationMs + 'ms');
      }
    }

    // ── Read FEE1 after (observational snapshot only, not proof) ──
    // Only attempt if still connected
    if (connected && fee1Char) {
      await sleep(100);
      try {
        var fee1After = await fee1Char.readValue();
        log('  FEE1 (after):  ' + bytesToHex(fee1After));
      } catch (err) {
        log('  FEE1 (after) read error: ' + (err.message || err), 'warn');
      }
    }
  } finally {
    writeInFlight = false;
    updateUIState();
  }
}

function sleep(ms) {
  return new Promise(function(resolve) { setTimeout(resolve, ms); });
}

/* ── UI state management ────────────────────────────────────────── */

function setAcquireEnabled(enabled) {
  btnPermitted.disabled = !enabled;
  btnChoose.disabled = !enabled;
  // getDevices may not be available
  if (enabled && typeof navigator.bluetooth.getDevices !== 'function') {
    btnPermitted.disabled = true;
    btnPermitted.title = 'getDevices() not supported in this browser';
  }
}

function updateUIState() {
  var acquireBlocked = connectInFlight || acquisitionInFlight;

  if (connected) {
    setAcquireEnabled(false);
    btnConnect.disabled = true;
    btnDisconnect.disabled = writeInFlight;
    connStateEl.textContent = 'connected ✓';
    connStateEl.style.color = '#0a6';

    // Read buttons: enabled only when ch exists AND properties.read is true
    btnReadFee1.disabled = !(fee1Char && fee1Char.properties && fee1Char.properties.read);
    btnReadBattery.disabled = !(batteryLevelChar && batteryLevelChar.properties && batteryLevelChar.properties.read);

    // Write gate: always visible when connected
    writeGateEl.style.display = 'block';

    updateCharStatus();
    updateWriteControls();
  } else {
    setAcquireEnabled(!acquireBlocked);
    btnConnect.disabled = acquireBlocked || !bluetoothDevice;
    btnDisconnect.disabled = true;
    connStateEl.textContent = connecting() ? 'connecting…' : 'disconnected';
    connStateEl.style.color = connecting() ? '#0cf' : '#888';

    btnReadFee1.disabled = true;
    btnReadBattery.disabled = true;

    // Hide write panel when disconnected
    writeGateEl.style.display = 'none';

    charStatusEl.textContent = '—';
    fee1ValEl.textContent = '—';
    batteryValEl.textContent = '—';
  }
}

function connecting() {
  return connectInFlight && !connected;
}

function updateCharStatus() {
  var parts = [];
  parts.push(fee1Char ? 'FEE1 ✓' : 'FEE1 ✗');
  parts.push(fee3Char ? 'FEE3 ✓' : 'FEE3 ✗');
  if (fee4Char) {
    parts.push(fee4Subscribed ? 'FEE4 ✓' : 'FEE4 ✗');
  } else {
    parts.push('FEE4 ✗');
  }
  parts.push(batteryLevelChar ? 'BATT ✓' : 'BATT ✗');
  charStatusEl.textContent = parts.join('  ');
}

function updateWriteControls() {
  var gatesMet = chkAckRisk.checked && txtConfirm.value.trim() === 'WRITE';
  var canW = canWrite();
  var pktResult = validatePacket(txtHex.value);
  var timeoutResult = validateAckTimeout();

  // Always update preview when connected
  if (pktResult.error) {
    pktPreviewEl.textContent = 'Error: ' + pktResult.error;
    pktPreviewEl.style.color = '#f66';
  } else if (pktResult.bytes) {
    pktPreviewEl.textContent = 'Preview: ' + bytesToHex(pktResult.bytes) +
      '  length=' + pktResult.bytes.length +
      '  RID=0x' + pktResult.reportId.toString(16).padStart(2, '0');
    pktPreviewEl.style.color = '#e0e0e0';
  }

  // Send button: enabled only when all gates satisfied AND FEE4 subscribed
  // AND valid packet AND valid timeout AND not in flight
  if (!connected) {
    btnSend.disabled = true;
    btnSend.textContent = 'Send Write';
  } else if (writeInFlight) {
    btnSend.disabled = true;
    btnSend.textContent = 'Writing…';
  } else if (!gatesMet) {
    btnSend.disabled = true;
    btnSend.textContent = 'Send Write';
  } else if (!canW.ok) {
    btnSend.disabled = true;
    btnSend.textContent = 'Send Write';
    if (!fee4Subscribed) {
      pktPreviewEl.textContent = 'Disabled: FEE4 notify not subscribed — ACK correlation unavailable.';
      pktPreviewEl.style.color = '#f90';
    }
  } else if (pktResult.error) {
    btnSend.disabled = true;
    btnSend.textContent = 'Send Write';
  } else if (timeoutResult.error) {
    btnSend.disabled = true;
    btnSend.textContent = 'Send Write';
    pktPreviewEl.textContent = 'ACK timeout error: ' + timeoutResult.error;
    pktPreviewEl.style.color = '#f90';
  } else {
    btnSend.disabled = false;
    btnSend.textContent = 'Send Write';
  }
}

function onDeviceReady(device, generation) {
  if (acquisitionGeneration !== generation) {
    // A newer acquisition started — discard this stale result
    log('(stale acquisition result discarded — newer operation started)');
    return;
  }
  if (connected) {
    log('(acquisition result discarded — already connected)');
    return;
  }
  if (!device) return;
  bluetoothDevice = device;
  devInfoEl.textContent = 'Ready: ' + (device.name || '(unnamed)') + '  id=' + (device.id || '?');
  btnConnect.disabled = false;
  log('Device acquired. Click Connect to establish GATT connection.');
}

/* ── event wiring ───────────────────────────────────────────────── */

function handlePermittedClick() {
  if (connectInFlight) { log('Connection in progress.', 'warn'); return; }
  if (acquisitionInFlight) { log('Acquisition already in progress.', 'warn'); return; }
  if (connected) { log('Disconnect first.'); return; }

  acquisitionInFlight = true;
  acquisitionGeneration++;
  var gen = acquisitionGeneration;
  updateUIState();

  acquirePermittedDevice().then(function(device) {
    onDeviceReady(device, gen);
  }).finally(function() {
    acquisitionInFlight = false;
    updateUIState();
  });
}

function handleChooseClick() {
  if (connectInFlight) { log('Connection in progress.', 'warn'); return; }
  if (acquisitionInFlight) { log('Acquisition already in progress.', 'warn'); return; }
  if (connected) { log('Disconnect first.'); return; }

  acquisitionInFlight = true;
  acquisitionGeneration++;
  var gen = acquisitionGeneration;
  updateUIState();

  acquireChooserDevice().then(function(device) {
    onDeviceReady(device, gen);
  }).finally(function() {
    acquisitionInFlight = false;
    updateUIState();
  });
}

function handleConnectClick() {
  if (connectInFlight) return;
  if (acquisitionInFlight) { log('Acquisition in progress — wait for it to complete.', 'warn'); return; }
  if (connected) return;
  if (!bluetoothDevice) {
    log('No device selected. Use "Use permitted device" or "Choose device" first.');
    return;
  }
  doConnect(bluetoothDevice);
}

function handleDisconnectClick() {
  doDisconnect();
}

function handleWriteGateChange() {
  if (connected) updateWriteControls();
}

function handleSendClick() {
  doWrite();
}

btnPermitted.addEventListener('click', handlePermittedClick);
btnChoose.addEventListener('click', handleChooseClick);
btnConnect.addEventListener('click', handleConnectClick);
btnDisconnect.addEventListener('click', handleDisconnectClick);
btnReadFee1.addEventListener('click', function() { readFee1(); });
btnReadBattery.addEventListener('click', function() { readBattery(); });

chkAckRisk.addEventListener('change', handleWriteGateChange);
txtConfirm.addEventListener('input', handleWriteGateChange);
txtHex.addEventListener('input', handleWriteGateChange);

btnSend.addEventListener('click', handleSendClick);

btnClearLog.addEventListener('click', function() {
  logEl.textContent = '';
});

btnCopyLog.addEventListener('click', function() {
  var text = logEl.textContent;
  if (text) {
    navigator.clipboard.writeText(text).then(function() {
      log('Log copied to clipboard.');
    }).catch(function(err) {
      log('Copy failed: ' + (err.message || err), 'error');
    });
  }
});

/* ── init ───────────────────────────────────────────────────────── */

function init() {
  // Check Web Bluetooth availability
  if (!navigator.bluetooth) {
    statusEl.textContent = 'Web Bluetooth NOT available (insecure context? not Chrome/Edge?)';
    statusEl.style.color = '#f66';
    log('Web Bluetooth unavailable — serve from localhost/HTTPS (secure context required).');
    setAcquireEnabled(false);
    btnConnect.disabled = true;
    return;
  }

  // Check for secure context
  if (!window.isSecureContext) {
    statusEl.textContent = 'NOT a secure context — Web Bluetooth requires HTTPS or localhost.';
    statusEl.style.color = '#f66';
    log('Insecure context — serve from localhost (e.g. npx serve web-bluetooth-probe) or HTTPS.');
    setAcquireEnabled(false);
    btnConnect.disabled = true;
    return;
  }

  // Check getDevices availability
  var hasGetDevices = typeof navigator.bluetooth.getDevices === 'function';

  statusEl.textContent = 'Web Bluetooth available ✓' +
    (hasGetDevices ? '  getDevices() supported' : '');
  statusEl.style.color = '#0a6';
  log('Web Bluetooth API detected.' + (hasGetDevices ? ' getDevices() available.' : ''));

  // Set default ACK timeout
  ackTimeoutInp.value = ACK_TIMEOUT_DEFAULT;

  // Initial UI state
  updateUIState();
  log('Page loaded. Waiting for user action.');
}

init();
