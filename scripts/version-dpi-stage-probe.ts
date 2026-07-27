#!/usr/bin/env bun

/**
 * Probe: does 0x0B byte [6] track the active DPI stage?
 *
 * Reads 0x0B, writes a different active DPI stage, reads 0x0B again.
 * If byte [6] changes, it tracks the DPI stage.
 *
 * Usage:
 *   bun scripts/version-dpi-stage-probe.ts [--out /tmp/version-dpi-stage.json]
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

function writeReport(device: HID.HID, packet: Buffer): void {
	device.sendFeatureReport(packet);
	sleep(SETTLE_MS);
}

function hex(raw: Buffer): string {
	return raw.toString('hex');
}

function describeVersion(raw: Buffer): string {
	return `hex=${hex(raw)}, [2]=${raw[2]}, [3]=0x${(raw[3] ?? 0).toString(16).padStart(2, '0')}, [4]=0x${(raw[4] ?? 0).toString(16).padStart(2, '0')}, [5]=0x${(raw[5] ?? 0).toString(16).padStart(2, '0')}, [6]=0x${(raw[6] ?? 0).toString(16).padStart(2, '0')}, [7]=0x${(raw[7] ?? 0).toString(16).padStart(2, '0')}`;
}

function describeDpi(raw: Buffer): string {
	const activeStage = raw[24];
	const stageCount = (() => {
		let count = 0;
		for (let i = 0; i < 8; i++) {
			const lo = raw[8 + i] ?? 0;
			const hi = raw[16 + i] ?? 0;
			if ((lo | (hi << 8)) > 0) count++;
		}
		return count;
	})();
	return `profile=${raw[2]}, activeStage=${activeStage}, stageCount=${stageCount}, hex=${hex(raw).slice(0, 40)}...`;
}

console.log('=== 0x0B byte [6] vs active DPI stage probe ===\n');

const device = discover();
const outPath = process.argv.includes('--out') ? process.argv[process.argv.indexOf('--out') + 1] : undefined;
const results: Record<string, unknown> = { steps: [] };

try {
	const step = (name: string, data: Record<string, unknown>) => {
		console.log(`── ${name} ──`);
		for (const [k, v] of Object.entries(data)) console.log(`  ${k}: ${v}`);
		(results.steps as Record<string, unknown>[]).push({ name, ...data });
	};

	// ── Step 0: Read current DPI state ─────────────────────────────────
	const dpi1 = armAndRead(device, 0x04, 56, 1);
	const dpi2 = armAndRead(device, 0x04, 56, 2);
	step('Current DPI state', {
		profile1: describeDpi(dpi1),
		profile2: describeDpi(dpi2),
	});

	const originalDpi1 = Buffer.from(dpi1);
	const originalDpi2 = Buffer.from(dpi2);
	const p1ActiveStage = dpi1[24] ?? 1;
	const p1StageCount = (() => {
		let count = 0;
		for (let i = 0; i < 8; i++) {
			const lo = dpi1[8 + i] ?? 0;
			const hi = dpi1[16 + i] ?? 0;
			if ((lo | (hi << 8)) > 0) count++;
		}
		return count;
	})();

	// Determine target stage (different from current)
	const targetStage = p1ActiveStage === 1 ? 2 : 1;
	if (targetStage > p1StageCount) {
		console.log(`  ⚠ Profile 1 only has ${p1StageCount} DPI stage(s); cannot switch to stage ${targetStage}.`);
		console.log(`  Writing 2 stages to profile 1 first...`);
		// Build a 2-stage DPI packet based on the current one
		const twoStage = Buffer.from(dpi1);
		// Enable 2 stages: mask = 0x03
		twoStage[5] = 0x03;
		// Set stage 2 to a different DPI (e.g., 800 = raw 15)
		twoStage[9] = 15; // stage 2 low byte (800 DPI)
		twoStage[17] = 0; // stage 2 high byte
		// Set active stage to 1
		twoStage[24] = 1;
		// Recalculate checksum
		let checksum = 0;
		for (let i = 3; i <= 49; i++) checksum = (checksum + (twoStage[i] ?? 0)) & 0xffff;
		twoStage[50] = (checksum >> 8) & 0xff;
		twoStage[51] = checksum & 0xff;
		writeReport(device, twoStage);
		console.log(`  Written 2-stage DPI: ${hex(twoStage).slice(0, 40)}...`);
	}

	// ── Step 1: Read 0x0B at current DPI stage ─────────────────────────
	const v_before = armAndRead(device, 0x0b, 8, 1);
	step('0x0B at current DPI stage', {
		version: describeVersion(v_before),
		dpi_active_stage: p1ActiveStage,
	});

	// ── Step 2: Write different active DPI stage ────────────────────────
	// Build DPI packet with different active stage
	const switchedDpi = Buffer.from(dpi1);
	switchedDpi[24] = targetStage;
	// Recalculate checksum
	let checksum = 0;
	for (let i = 3; i <= 49; i++) checksum = (checksum + (switchedDpi[i] ?? 0)) & 0xffff;
	switchedDpi[50] = (checksum >> 8) & 0xff;
	switchedDpi[51] = checksum & 0xff;

	console.log(`\n  Writing DPI with active stage ${targetStage}: ${hex(switchedDpi).slice(0, 40)}...`);
	writeReport(device, switchedDpi);

	// Verify the write
	const dpi_after_write = armAndRead(device, 0x04, 56, 1);
	step('DPI after active stage write', {
		dpi: describeDpi(dpi_after_write),
		verified: dpi_after_write[24] === targetStage ? 'YES' : 'NO',
	});

	// ── Step 3: Read 0x0B at new DPI stage ─────────────────────────────
	const v_after = armAndRead(device, 0x0b, 8, 1);
	step('0x0B at new DPI stage', {
		version: describeVersion(v_after),
		dpi_active_stage: targetStage,
	});

	// ── Step 4: Switch back and read again ──────────────────────────────
	const restoreDpi = Buffer.from(dpi1);
	restoreDpi[24] = p1ActiveStage;
	checksum = 0;
	for (let i = 3; i <= 49; i++) checksum = (checksum + (restoreDpi[i] ?? 0)) & 0xffff;
	restoreDpi[50] = (checksum >> 8) & 0xff;
	restoreDpi[51] = checksum & 0xff;

	console.log(`\n  Restoring DPI active stage ${p1ActiveStage}...`);
	writeReport(device, restoreDpi);

	const v_restored = armAndRead(device, 0x0b, 8, 1);
	step('0x0B after DPI stage restore', {
		version: describeVersion(v_restored),
		dpi_active_stage: p1ActiveStage,
	});

	// ── Analysis ───────────────────────────────────────────────────────
	console.log('\n── Analysis ──');

	const byte6_before = v_before[6];
	const byte6_after = v_after[6];
	const byte6_restored = v_restored[6];
	console.log(`  byte[6] at stage ${p1ActiveStage}:      0x${(byte6_before ?? 0).toString(16).padStart(2, '0')}`);
	console.log(`  byte[6] at stage ${targetStage}:      0x${(byte6_after ?? 0).toString(16).padStart(2, '0')}`);
	console.log(`  byte[6] restored stage ${p1ActiveStage}: 0x${(byte6_restored ?? 0).toString(16).padStart(2, '0')}`);

	if (byte6_before !== byte6_after) {
		console.log(`\n  ✅ byte[6] CHANGED — it tracks the active DPI stage!`);
		console.log(`    stage ${p1ActiveStage} → 0x${(byte6_before ?? 0).toString(16).padStart(2, '0')}`);
		console.log(`    stage ${targetStage} → 0x${(byte6_after ?? 0).toString(16).padStart(2, '0')}`);
	} else {
		console.log(`\n  ❌ byte[6] unchanged — it does NOT track the active DPI stage.`);
	}

	// Check other bytes too
	console.log('\n  Other bytes:');
	for (const idx of [2, 3, 4, 5, 7]) {
		const before = v_before[idx];
		const after = v_after[idx];
		const changed = before !== after;
		console.log(
			`    byte[${idx}]: 0x${(before ?? 0).toString(16).padStart(2, '0')} → 0x${(after ?? 0).toString(16).padStart(2, '0')} ${changed ? '← CHANGED' : '(same)'}`,
		);
	}

	(results as { analysis: Record<string, unknown> }).analysis = {
		byte6_before,
		byte6_after,
		byte6_restored,
		byte6_tracks_dpi_stage: byte6_before !== byte6_after,
		dpi_stage_before: p1ActiveStage,
		dpi_stage_after: targetStage,
	};

	// ── Restore original DPI ───────────────────────────────────────────
	console.log('\n── Restoring original DPI ──');
	writeReport(device, originalDpi1);
	const finalDpi = armAndRead(device, 0x04, 56, 1);
	console.log(`  Restored: ${hex(finalDpi) === hex(originalDpi1) ? '✅' : '❌'}`);
} finally {
	device.close();
}

if (outPath) {
	await Bun.write(outPath, `${JSON.stringify(results, null, 2)}\n`);
	console.log(`\nWritten to ${outPath}`);
}
