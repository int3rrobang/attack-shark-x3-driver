#!/usr/bin/env bun

/**
 * Probe: which bytes in 0x0B are profile-dependent?
 *
 * Sets up distinguishable profiles (different light modes, max=2), then
 * reads 0x0B with different selectors to see which bytes move.
 *
 * Test sequence:
 *   1. Read current state (metadata, prefs for profiles 1 & 2)
 *   2. Set max=2 via 0x0C
 *   3. Write light mode 0x10 to profile 1, 0x70 to profile 2
 *   4. Read 0x0B selector=1, then selector=2
 *   5. Compare all bytes
 *   6. Restore original state
 *
 * Usage:
 *   bun scripts/version-byte-probe.ts [--out /tmp/version-byte.json]
 */

import * as HID from 'node-hid';

const VID = 0x1d57;
const PID = 0xfa61;
const COL04 = /col04/i;
const SETTLE_MS = 750;

function sleep(ms: number): void {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function discover(): HID.HID {
	const candidates = (HID.devices() as HID.Device[]).filter(
		(d) => d.vendorId === VID && d.productId === PID && COL04.test(d.path ?? ''),
	);
	if (candidates.length !== 1)
		throw new Error(candidates.length === 0 ? 'No device found' : `Found ${candidates.length}; unplug extras`);
	const c = candidates[0]!;
	if (!c.path) throw new Error('No path');
	return new HID.HID(c.path);
}

function armAndRead(device: HID.HID, reportId: number, reportLen: number, profile: number): Buffer {
	const selector = Buffer.from([0xa0, reportId, reportLen, 0x00, profile, 0x00, 0x00, 0x00]);
	device.sendFeatureReport(selector);
	sleep(SETTLE_MS);
	const ready = device.getFeatureReport(0xa0, 8);
	if (ready.length < 2 || ready[0] !== 0xa0 || ready[1] !== 0x01)
		throw new Error(`Readiness failed for 0x${reportId.toString(16)}: ${Buffer.from(ready).toString('hex')}`);
	return Buffer.from(device.getFeatureReport(reportId, reportLen));
}

function writeReport(device: HID.HID, packet: Buffer): void {
	device.sendFeatureReport(packet);
	sleep(SETTLE_MS);
}

function hex(raw: Buffer): string {
	return raw.toString('hex');
}

function describeVersion(raw: Buffer): string {
	return [
		`hex=${hex(raw)}`,
		`[0]=0x${(raw[0] ?? 0).toString(16).padStart(2, '0')} (reportId)`,
		`[1]=0x${(raw[1] ?? 0).toString(16).padStart(2, '0')} (declaredLen)`,
		`[2]=0x${(raw[2] ?? 0).toString(16).padStart(2, '0')} (selector echo)`,
		`[3]=0x${(raw[3] ?? 0).toString(16).padStart(2, '0')}`,
		`[4]=0x${(raw[4] ?? 0).toString(16).padStart(2, '0')}`,
		`[5]=0x${(raw[5] ?? 0).toString(16).padStart(2, '0')}`,
		`[6]=0x${(raw[6] ?? 0).toString(16).padStart(2, '0')}`,
		`[7]=0x${(raw[7] ?? 0).toString(16).padStart(2, '0')}`,
	].join(', ');
}

function describePrefs(raw: Buffer): string {
	return `hex=${hex(raw)}, profile=${raw[2]}, lightMode=0x${(raw[3] ?? 0).toString(16).padStart(2, '0')}, config=0x${(raw[4] ?? 0).toString(16).padStart(2, '0')}`;
}

function describeMetadata(raw: Buffer): string {
	return `hex=${hex(raw)}, subtype=${raw[2]}, current=${raw[3]}, ~current=${raw[4]}, max=${raw[5]}, ~max=${raw[6]}`;
}

/** Change only the light mode byte in a preferences packet, fix checksum. */
function withLightMode(prefs: Buffer, lightMode: number): Buffer {
	const packet = Buffer.from(prefs);
	packet[3] = lightMode;
	let checksum = 0;
	for (let i = 3; i <= 10; i++) checksum = (checksum + (packet[i] ?? 0)) & 0xffff;
	packet[11] = (checksum >> 8) & 0xff;
	packet[12] = checksum & 0xff;
	return packet;
}

/** Build a 0x0C profile-control packet (6-byte compact write). */
function buildProfileControl(current: number, maximum: number): Buffer {
	return Buffer.from([0x0c, 0x0a, current, 0xff - current, maximum, 0xff - maximum]);
}

console.log('=== 0x0B byte characterization probe ===\n');

const device = discover();
const outPath = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : undefined;
const results: Record<string, unknown> = { steps: [] };

try {
	const step = (name: string, data: Record<string, unknown>) => {
		console.log(`── ${name} ──`);
		for (const [k, v] of Object.entries(data)) console.log(`  ${k}: ${v}`);
		(results.steps as Record<string, unknown>[]).push({ name, ...data });
	};

	// ── Step 0: Read current state ─────────────────────────────────────
	const metadata = armAndRead(device, 0x0c, 10, 1);
	const prefs1 = armAndRead(device, 0x05, 15, 1);
	const prefs2 = armAndRead(device, 0x05, 15, 2);
	step('Initial state', {
		metadata: describeMetadata(metadata),
		prefs1: describePrefs(prefs1),
		prefs2: describePrefs(prefs2),
	});

	const originalCurrent = metadata[3] ?? 1;
	const originalMax = metadata[5] ?? 1;
	const originalLightMode1 = prefs1[3] ?? 0;
	const originalLightMode2 = prefs2[3] ?? 0;

	console.log(`\n  Will set max=2, light mode profile1=0x10, profile2=0x70`);
	console.log(
		`  (original: current=${originalCurrent}, max=${originalMax}, lm1=0x${originalLightMode1.toString(16).padStart(2, '0')}, lm2=0x${originalLightMode2.toString(16).padStart(2, '0')})\n`,
	);

	// ── Step 1: Set max=2 ──────────────────────────────────────────────
	const ctrl = buildProfileControl(1, 2);
	console.log(`  Writing 0x0C: ${hex(ctrl)}`);
	writeReport(device, ctrl);
	sleep(SETTLE_MS);

	const metadata_after_ctrl = armAndRead(device, 0x0c, 10, 1);
	step('After 0x0C max=2', { metadata: describeMetadata(metadata_after_ctrl) });

	// ── Step 2: Write distinct light modes ──────────────────────────────
	// Profile 1: light mode 0x10
	const p1_lm10 = withLightMode(prefs1, 0x10);
	console.log(`  Writing prefs profile 1 (light=0x10): ${hex(p1_lm10)}`);
	writeReport(device, p1_lm10);

	// Profile 2: light mode 0x70
	const p2_lm70 = withLightMode(prefs2, 0x70);
	console.log(`  Writing prefs profile 2 (light=0x70): ${hex(p2_lm70)}`);
	writeReport(device, p2_lm70);

	// Verify both
	const prefs1_after = armAndRead(device, 0x05, 15, 1);
	const prefs2_after = armAndRead(device, 0x05, 15, 2);
	step('After light mode writes', {
		prefs1: describePrefs(prefs1_after),
		prefs2: describePrefs(prefs2_after),
		verified: prefs1_after[3] === 0x10 && prefs2_after[3] === 0x70 ? 'YES' : 'NO',
	});

	// ── Step 3: Read 0x0B with different selectors ─────────────────────
	const v_sel1 = armAndRead(device, 0x0b, 8, 1);
	step('0x0B selector=1', { version: describeVersion(v_sel1) });

	const v_sel2 = armAndRead(device, 0x0b, 8, 2);
	step('0x0B selector=2', { version: describeVersion(v_sel2) });

	// ── Step 4: Also read 0x0B with selector=3 (max is 2, profile 3 doesn't exist) ──
	const v_sel3 = armAndRead(device, 0x0b, 8, 3);
	step('0x0B selector=3 (beyond max)', { version: describeVersion(v_sel3) });

	// ── Byte-by-byte comparison ────────────────────────────────────────
	console.log('\n── Byte-by-byte comparison ──');
	console.log('  Byte | sel=1      | sel=2      | sel=3      | Profile-dependent?');
	console.log('  -----|------------|------------|------------|-------------------');

	for (let i = 0; i < 8; i++) {
		const b1 = (v_sel1[i] ?? 0).toString(16).padStart(2, '0');
		const b2 = (v_sel2[i] ?? 0).toString(16).padStart(2, '0');
		const b3 = (v_sel3[i] ?? 0).toString(16).padStart(2, '0');
		const differs = b1 !== b2 || b1 !== b3;
		const marker = differs ? '← YES' : '';
		console.log(`  [${i}]   | 0x${b1}       | 0x${b2}       | 0x${b3}       | ${marker}`);
	}

	// ── Analysis ───────────────────────────────────────────────────────
	console.log('\n── Analysis ──');

	const byte2_sel1 = v_sel1[2];
	const byte2_sel2 = v_sel2[2];
	const byte4_sel1 = v_sel1[4];
	const byte4_sel2 = v_sel2[4];

	console.log(
		`  byte[2] = selector echo (sel1→0x${(byte2_sel1 ?? 0).toString(16)}, sel2→0x${(byte2_sel2 ?? 0).toString(16)})`,
	);
	console.log(
		`  byte[4] = light mode for selected profile (sel1→0x${(byte4_sel1 ?? 0).toString(16)}, sel2→0x${(byte4_sel2 ?? 0).toString(16)})`,
	);

	// Check bytes [3], [5], [6], [7]
	for (const idx of [3, 5, 6, 7]) {
		const vals = [v_sel1[idx], v_sel2[idx], v_sel3[idx]];
		const allSame = vals.every((v) => v === vals[0]);
		console.log(
			`  byte[${idx}] = 0x${(vals[0] ?? 0).toString(16).padStart(2, '0')} across all selectors ${allSame ? '(global/constant)' : '(CHANGES — profile-dependent!)'}`,
		);
	}

	(results as { analysis: Record<string, unknown> }).analysis = {
		byte2_selector_echo: true,
		byte4_light_mode: true,
		bytes_3_5_6_7: Object.fromEntries(
			[3, 5, 6, 7].map((i) => [
				`byte${i}`,
				{
					sel1: v_sel1[i],
					sel2: v_sel2[i],
					sel3: v_sel3[i],
					profile_dependent: v_sel1[i] !== v_sel2[i] || v_sel1[i] !== v_sel3[i],
				},
			]),
		),
	};

	// ── Restore ────────────────────────────────────────────────────────
	console.log('\n── Restoring original state ──');

	// Restore light modes
	if (originalLightMode1 !== 0x10) {
		writeReport(device, withLightMode(prefs1_after, originalLightMode1));
	}
	if (originalLightMode2 !== 0x70) {
		writeReport(device, withLightMode(prefs2_after, originalLightMode2));
	}

	// Restore max profile
	if (originalMax !== 2) {
		const restore_ctrl = buildProfileControl(originalCurrent, originalMax);
		console.log(`  Restoring 0x0C: ${hex(restore_ctrl)}`);
		writeReport(device, restore_ctrl);
	}

	// Verify
	const final_metadata = armAndRead(device, 0x0c, 10, 1);
	const final_prefs1 = armAndRead(device, 0x05, 15, 1);
	step('Restored state', {
		metadata: describeMetadata(final_metadata),
		prefs1: describePrefs(final_prefs1),
	});
} finally {
	device.close();
}

if (outPath) {
	await Bun.write(outPath, `${JSON.stringify(results, null, 2)}\n`);
	console.log(`\nWritten to ${outPath}`);
}
