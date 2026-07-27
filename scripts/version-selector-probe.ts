#!/usr/bin/env bun

/**
 * Probe: does the 0x0B selector byte load the working profile?
 *
 * Ensures profiles 1 and 2 have distinguishable DPI, then tests whether
 * reading 0x0B with a profile-2 selector contaminates profile 1's DPI.
 *
 * Test sequence:
 *   1. Read DPI for profiles 1 and 2 — confirm they differ (or write a marker)
 *   2. Read 0x0B selector=1, then 0x0B selector=2 → compare byte [4]
 *   3. Read DPI selector=1 → baseline
 *   4. Read 0x0B selector=2 → does this load profile 2?
 *   5. Read DPI selector=1 → contamination check
 *   6. Restore profile 2's original DPI if we wrote a marker
 *
 * Usage:
 *   bun scripts/version-selector-probe.ts [--out /tmp/version-selector.json]
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
		throw new Error(`Readiness failed: ${Buffer.from(ready).toString('hex')}`);
	return Buffer.from(device.getFeatureReport(reportId, reportLen));
}

function describeVersion(raw: Buffer): string {
	return [
		`raw=${raw.toString('hex')}`,
		`declaredLen=0x${(raw[1] ?? 0).toString(16).padStart(2, '0')}`,
		`profile=${raw[2]}`,
		`byte4=0x${(raw[4] ?? 0).toString(16).padStart(2, '0')}`,
		`byte5=0x${(raw[5] ?? 0).toString(16).padStart(2, '0')}`,
		`byte6=0x${(raw[6] ?? 0).toString(16).padStart(2, '0')}`,
		`byte7=0x${(raw[7] ?? 0).toString(16).padStart(2, '0')}`,
	].join(', ');
}

function describeDpi(raw: Buffer): string {
	const activeStage = raw[24];
	const stages: number[] = [];
	for (let i = 0; i < 8; i++) {
		const lo = raw[8 + i] ?? 0;
		const hi = raw[16 + i] ?? 0;
		const raw16 = lo | (hi << 8);
		if (raw16 > 0) stages.push((raw16 + 1) * 50);
	}
	return `profile=${raw[2]}, activeStage=${activeStage}, stages=[${stages.join(',')}], hex=${raw.toString('hex').slice(0, 40)}...`;
}

function dpiHex(raw: Buffer): string {
	return raw.toString('hex');
}

/** Build a DPI packet with a distinctive active stage for profile 2. */
function buildMarkerDpi(original: Buffer, markerStage: number): Buffer {
	const packet = Buffer.from(original);
	// Set active stage to markerStage (one-based)
	packet[24] = markerStage;
	// Recalculate checksum over bytes 3..49
	let checksum = 0;
	for (let i = 3; i <= 49; i++) checksum = (checksum + (packet[i] ?? 0)) & 0xffff;
	packet[50] = (checksum >> 8) & 0xff;
	packet[51] = checksum & 0xff;
	return packet;
}

console.log('=== 0x0B selector side-effect probe ===\n');

const device = discover();
const outPath = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : undefined;
const results: Record<string, unknown> = { steps: [] };

try {
	const step = (name: string, data: Record<string, unknown>) => {
		console.log(`── ${name} ──`);
		for (const [k, v] of Object.entries(data)) console.log(`  ${k}: ${v}`);
		(results.steps as Record<string, unknown>[]).push({ name, ...data });
	};

	// ── Step 0: Check if profiles have distinguishable DPI ─────────────
	const dpi_p1 = armAndRead(device, 0x04, 56, 1);
	const dpi_p2 = armAndRead(device, 0x04, 56, 2);
	step('DPI profiles 1 & 2', {
		profile1: describeDpi(dpi_p1),
		profile2: describeDpi(dpi_p2),
	});

	let wroteMarker = false;
	let originalDpi2: Buffer | null = null;

	if (dpiHex(dpi_p1) === dpiHex(dpi_p2)) {
		console.log('\n  ⚠ Profiles have identical DPI — writing a marker to profile 2...\n');
		originalDpi2 = Buffer.from(dpi_p2);

		// Find a stage that differs from profile 1's active stage
		const p1Active = dpi_p1[24] ?? 1;
		const markerStage = p1Active === 1 ? 2 : 1;
		const marker = buildMarkerDpi(dpi_p2, markerStage);

		console.log(`  Writing marker: active stage ${markerStage} for profile 2`);
		device.sendFeatureReport(marker);
		sleep(SETTLE_MS);

		// Verify the write took effect
		const dpi_p2_after = armAndRead(device, 0x04, 56, 2);
		step('DPI profile 2 after marker write', {
			dpi: describeDpi(dpi_p2_after),
			verified: dpiHex(dpi_p2_after) !== dpiHex(dpi_p1),
		});

		if (dpiHex(dpi_p2_after) === dpiHex(dpi_p1)) {
			console.log('  ❌ Marker write did not differentiate profiles — aborting.\n');
			process.exit(1);
		}
		wroteMarker = true;

		// Re-read profile 1 DPI to get the post-write baseline
		// (the marker write shouldn't have affected profile 1, but be safe)
	}

	// ── Step 1: 0x0B with different selectors ──────────────────────────
	const v_p1 = armAndRead(device, 0x0b, 8, 1);
	step('0x0B selector=profile1', { version: describeVersion(v_p1) });

	const v_p2 = armAndRead(device, 0x0b, 8, 2);
	step('0x0B selector=profile2', { version: describeVersion(v_p2) });

	// ── Step 2: DPI baseline for profile 1 ─────────────────────────────
	const dpi_baseline = armAndRead(device, 0x04, 56, 1);
	step('DPI profile 1 baseline', { dpi: describeDpi(dpi_baseline) });

	// ── Step 3: Read 0x0B with profile 2 selector ──────────────────────
	const v_p2_again = armAndRead(device, 0x0b, 8, 2);
	step('0x0B selector=profile2 (again)', { version: describeVersion(v_p2_again) });

	// ── Step 4: DPI profile 1 — contamination check ────────────────────
	const dpi_after = armAndRead(device, 0x04, 56, 1);
	step('DPI profile 1 after 0x0B prof2 read', { dpi: describeDpi(dpi_after) });

	// ── Step 5: 0x0B profile 1 final ───────────────────────────────────
	const v_p1_final = armAndRead(device, 0x0b, 8, 1);
	step('0x0B selector=profile1 (final)', { version: describeVersion(v_p1_final) });

	// ── Analysis ───────────────────────────────────────────────────────
	console.log('\n── Analysis ──');

	const byte4_p1 = v_p1[4];
	const byte4_p2 = v_p2[4];
	const byte4_p1_final = v_p1_final[4];
	console.log(`  byte[4] profile1:       0x${(byte4_p1 ?? 0).toString(16).padStart(2, '0')}`);
	console.log(`  byte[4] profile2:       0x${(byte4_p2 ?? 0).toString(16).padStart(2, '0')}`);
	console.log(`  byte[4] profile1 final: 0x${(byte4_p1_final ?? 0).toString(16).padStart(2, '0')}`);
	console.log(`  byte[4] differs between profiles: ${byte4_p1 !== byte4_p2 ? 'YES' : 'NO'}`);

	const dpi_changed = dpiHex(dpi_baseline) !== dpiHex(dpi_after);
	console.log(
		`\n  DPI profile 1 changed after 0x0B prof2 read: ${dpi_changed ? '⚠ YES — SELECTOR LOADS PROFILE' : '✅ NO — selector is inert'}`,
	);

	if (dpi_changed) {
		console.log(`  Before: ${dpi_baseline.toString('hex').slice(0, 40)}...`);
		console.log(`  After:  ${dpi_after.toString('hex').slice(0, 40)}...`);
	}

	(results as { analysis: Record<string, unknown> }).analysis = {
		byte4_p1,
		byte4_p2,
		byte4_p1_final,
		byte4_differs: byte4_p1 !== byte4_p2,
		dpi_changed,
		wroteMarker,
	};

	// ── Restore profile 2 if we wrote a marker ─────────────────────────
	if (wroteMarker && originalDpi2) {
		console.log('\n── Restoring profile 2 original DPI ──');
		device.sendFeatureReport(originalDpi2);
		sleep(SETTLE_MS);
		const restored = armAndRead(device, 0x04, 56, 2);
		const match = dpiHex(restored) === dpiHex(originalDpi2);
		console.log(`  Restored: ${match ? '✅' : '❌'}`);
	}
} finally {
	device.close();
}

if (outPath) {
	await Bun.write(outPath, `${JSON.stringify(results, null, 2)}\n`);
	console.log(`\nWritten to ${outPath}`);
}
