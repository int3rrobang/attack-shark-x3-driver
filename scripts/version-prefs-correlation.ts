#!/usr/bin/env bun

/**
 * Quick check: does 0x0B byte [4] track the light-mode byte from 0x05?
 *
 * Reads both reports, then does a reversible light-mode toggle to verify
 * the correlation is live and not coincidental.
 *
 * Usage:
 *   bun scripts/version-prefs-correlation.ts [--write]
 *
 * Without --write: read-only correlation check.
 * With --write: toggle light mode 0x10 → 0x00 → 0x10 and observe 0x0B.
 */

import * as HID from 'node-hid';

const VID = 0x1d57;
const PID = 0xfa61;
const COL04 = /col04/i;
const SETTLE_MS = 750;

const VERSION_ID = 0x0b;
const VERSION_LEN = 0x08;
const PREFS_ID = 0x05;
const PREFS_LEN = 0x0f;
const SELECTOR_ID = 0xa0;
const READY_LEN = 0x08;

function sleep(ms: number): void {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function discover(): HID.HID {
	const candidates = (HID.devices() as HID.Device[]).filter(
		(d) => d.vendorId === VID && d.productId === PID && COL04.test(d.path ?? ''),
	);
	if (candidates.length !== 1) {
		throw new Error(
			candidates.length === 0
				? 'No FA61 Col04 device found'
				: `Found ${candidates.length} devices; unplug extras`,
		);
	}
	const c = candidates[0]!;
	if (!c.path) throw new Error('No path');
	return new HID.HID(c.path);
}

function armAndRead(device: HID.HID, reportId: number, reportLen: number): Buffer {
	const selector = Buffer.from([SELECTOR_ID, reportId, reportLen, 0x00, 0x01, 0x00, 0x00, 0x00]);
	device.sendFeatureReport(selector);
	sleep(SETTLE_MS);
	const ready = device.getFeatureReport(SELECTOR_ID, READY_LEN);
	if (ready.length < 2 || ready[0] !== SELECTOR_ID || ready[1] !== 0x01) {
		throw new Error(`Readiness failed for 0x${reportId.toString(16)}: ${Buffer.from(ready).toString('hex')}`);
	}
	return Buffer.from(device.getFeatureReport(reportId, reportLen));
}

function describeVersion(raw: Buffer): string {
	return [
		`report=${raw[0]?.toString(16).padStart(2, '0')}`,
		`declaredLen=${raw[1]?.toString(16).padStart(2, '0')}`,
		`profile=${raw[2]}`,
		`byte4=${raw[4]?.toString(16).padStart(2, '0')}`,
		`byte5=${raw[5]?.toString(16).padStart(2, '0')}`,
		`byte6=${raw[6]?.toString(16).padStart(2, '0')}`,
		`byte7=${raw[7]?.toString(16).padStart(2, '0')}`,
	].join(', ');
}

function describePrefs(raw: Buffer): string {
	const profile = raw[2];
	const lightMode = raw[3];
	const config = raw[4];
	const deepSleep = raw[5];
	return [
		`profile=${profile}`,
		`lightMode=0x${lightMode?.toString(16).padStart(2, '0')}`,
		`config=0x${config?.toString(16).padStart(2, '0')}`,
		`deepSleep=0x${deepSleep?.toString(16).padStart(2, '0')}`,
	].join(', ');
}

const doWrite = process.argv.includes('--write');

console.log('=== 0x0B / 0x05 correlation check ===\n');

const device = discover();
try {
	// ── Read both reports ───────────────────────────────────────────────
	const version = armAndRead(device, VERSION_ID, VERSION_LEN);
	const prefs = armAndRead(device, PREFS_ID, PREFS_LEN);

	console.log(`0x0B version:   ${version.toString('hex')}`);
	console.log(`  → ${describeVersion(version)}`);
	console.log();
	console.log(`0x05 prefs:     ${prefs.toString('hex')}`);
	console.log(`  → ${describePrefs(prefs)}`);
	console.log();

	const versionByte4 = version[4] ?? 0;
	const prefsLightMode = prefs[3] ?? 0;

	console.log(`Correlation:`);
	console.log(`  0x0B byte[4]          = 0x${versionByte4.toString(16).padStart(2, '0')}`);
	console.log(`  0x05 light mode byte  = 0x${prefsLightMode.toString(16).padStart(2, '0')}`);
	console.log(`  Match: ${versionByte4 === prefsLightMode ? '✅ YES' : '❌ NO'}`);

	if (!doWrite) {
		console.log('\n(Read-only pass.  Run with --write to test live tracking.)');
	} else {
		// ── Write test: toggle light mode ──────────────────────────────
		console.log('\n── Write test: toggle light mode ──\n');

		const originalLightMode = prefsLightMode;
		const toggleLightMode = originalLightMode === 0x00 ? 0x10 : 0x00;
		console.log(
			`  Toggling light mode: 0x${originalLightMode.toString(16).padStart(2, '0')} → 0x${toggleLightMode.toString(16).padStart(2, '0')}`,
		);

		// Build modified preferences packet (preserve everything, change only light mode).
		const modified = Buffer.from(prefs);
		modified[3] = toggleLightMode;
		// Recalculate 16-bit checksum over bytes 3..10.
		let checksum = 0;
		for (let i = 3; i <= 10; i++) checksum = (checksum + (modified[i] ?? 0)) & 0xffff;
		modified[11] = (checksum >> 8) & 0xff;
		modified[12] = checksum & 0xff;

		console.log(`  Writing: ${modified.toString('hex')}`);
		device.sendFeatureReport(modified);
		sleep(SETTLE_MS);

		// Re-read both.
		const version2 = armAndRead(device, VERSION_ID, VERSION_LEN);
		const prefs2 = armAndRead(device, PREFS_ID, PREFS_LEN);

		console.log();
		console.log(`  After write:`);
		console.log(`    0x0B version:   ${version2.toString('hex')}`);
		console.log(`    0x05 prefs:     ${prefs2.toString('hex')}`);
		console.log(`    0x0B byte[4]          = 0x${(version2[4] ?? 0).toString(16).padStart(2, '0')}`);
		console.log(`    0x05 light mode byte  = 0x${(prefs2[3] ?? 0).toString(16).padStart(2, '0')}`);
		console.log(`    Match: ${(version2[4] ?? 0) === (prefs2[3] ?? 0) ? '✅ YES' : '❌ NO'}`);
		console.log(
			`    Byte [4] changed: ${(version2[4] ?? 0) !== versionByte4 ? '✅ YES (tracks!)' : '❌ NO (static)'}`,
		);

		// Restore.
		console.log(`\n  Restoring original light mode 0x${originalLightMode.toString(16).padStart(2, '0')}...`);
		const restored = Buffer.from(prefs);
		device.sendFeatureReport(restored);
		sleep(SETTLE_MS);

		const version3 = armAndRead(device, VERSION_ID, VERSION_LEN);
		const prefs3 = armAndRead(device, PREFS_ID, PREFS_LEN);
		console.log(
			`    0x0B byte[4] = 0x${(version3[4] ?? 0).toString(16).padStart(2, '0')} (expected 0x${originalLightMode.toString(16).padStart(2, '0')})`,
		);
		console.log(`    Restored: ${(version3[4] ?? 0) === originalLightMode ? '✅ YES' : '❌ NO'}`);
	}
} finally {
	device.close();
}
