#!/usr/bin/env bun

/**
 * Report 0x0A bulk-profile field-mapping probe.
 *
 * Reads the 128-byte 0x0A report before and after controlled, reversible
 * changes to known report fields, then diffs each pair to map which
 * regions of 0x0A correspond to which report.
 *
 * All changes are applied to the same profile and restored after each test.
 *
 * Tests (in order):
 *   1. DPI active stage: 1 → 2
 *   2. DPI value: stage 1 value change (e.g. 800 → 1600)
 *   3. Preferences light mode: 0x00 ↔ 0x10
 *   4. Preferences debounce: change debounce byte
 *   5. Preferences sleep timer: change sleep byte
 *   6. Button assignment: flip one slot byte
 *   7. Profile metadata: switch current profile (0x0c write) — most invasive
 *
 * Usage:
 *   bun scripts/bulk-profile-probe.ts --out C:/temp/bulk-profile.json
 *   bun scripts/bulk-profile-probe.ts --out C:/temp/bulk-profile.json --profile 2
 */

import * as HID from 'node-hid';
import { parseArgs } from 'node:util';
import { buildReadSelector, decodeProfileMetadata, FA61_REPORT_LENGTH } from '../src/experimental/Fa61ProfileProbe.js';

// ── Constants ────────────────────────────────────────────────────────────────

const VID = 0x1d57;
const FA61_PID = 0xfa61;
const FA60_PID = 0xfa60;
const COL04 = /col04/i;
const SETTLE_MS = 750;
const RESTORE_SETTLE_MS = 2000;

const BULK_REPORT_ID = 0x0a;
const BULK_LENGTH = FA61_REPORT_LENGTH.BULK_PROFILE; // 0x80 = 128

// ── CLI ──────────────────────────────────────────────────────────────────────

const HELP = `Bulk profile (0x0A) field-mapping probe

Reads the 128-byte 0x0A report before and after controlled changes to DPI,
preferences, buttons, and profile metadata, then diffs each pair.

Options:
  --transport <mode>  wired | receiver  (default: wired)
  --out <path>        Output JSON path  (required)
  --profile <n>       Target profile  (default: 1)
  --help              Show this help
`;
const parsed = parseArgs({
	allowPositionals: false,
	options: {
		transport: { type: 'string' },
		out: { type: 'string' },
		profile: { type: 'string' },
		help: { type: 'boolean' },
	},
});

if (parsed.values.help) {
	console.log(HELP);
	process.exit(0);
}

const transport = (() => {
	const raw = parsed.values.transport;
	if (raw === undefined || raw === 'wired') return 'wired' as const;
	if (raw === 'receiver') return 'receiver' as const;
	throw new Error('--transport must be wired or receiver');
})();

const targetPid = transport === 'receiver' ? FA60_PID : FA61_PID;
const transportLabel = transport === 'receiver' ? 'FA60 receiver' : 'FA61 wired';

const outPath = parsed.values.out;
if (!outPath) throw new Error('--out <path> is required');

const targetProfile = (() => {
	const raw = parsed.values.profile;
	if (raw === undefined) return 1;
	const n = Number.parseInt(raw, 10);
	if (!Number.isInteger(n) || n < 1 || n > 5) throw new Error('--profile must be 1–5');
	return n;
})();

// ── HID helpers ──────────────────────────────────────────────────────────────

function sleepMs(ms: number): void {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function discover(): HID.HID {
	const candidates = (HID.devices() as HID.Device[]).filter(
		(d) => d.vendorId === VID && d.productId === targetPid && COL04.test(d.path ?? ''),
	);
	if (candidates.length !== 1) {
		throw new Error(
			candidates.length === 0
				? `No ${transportLabel} Col04 device found`
				: `Found ${candidates.length} devices; unplug extras`,
		);
	}
	const c = candidates[0]!;
	if (!c.path) throw new Error('No path');
	return new HID.HID(c.path);
}

function readReport(device: HID.HID, reportId: number, reportLen: number, profile?: number): Buffer {
	const selectorProfile = profile ?? 1;
	const selector = buildReadSelector(reportId, reportLen, selectorProfile);
	device.sendFeatureReport(selector);

	// Initial settle — match the Rust driver's receiver policy.
	sleepMs(100);

	// Poll readiness mailbox (matches Rust driver: 5ms interval, 2s timeout).
	const deadline = Date.now() + 2000;
	let ready: Buffer = Buffer.alloc(0);
	while (Date.now() < deadline) {
		ready = Buffer.from(device.getFeatureReport(0xa0, 0x08));
		if (ready.length >= 2 && ready[0] === 0xa0 && ready[1] === 0x01) break;
		sleepMs(5);
	}
	if (ready.length < 2 || ready[0] !== 0xa0 || ready[1] !== 0x01) {
		throw new Error(`Readiness timeout for 0x${reportId.toString(16)}: ${ready.toString('hex')}`);
	}
	return Buffer.from(device.getFeatureReport(reportId, reportLen));
}
function writeReport(device: HID.HID, packet: Buffer): void {
	device.sendFeatureReport(packet);
	sleepMs(RESTORE_SETTLE_MS);
}

// ── Checksum helpers ─────────────────────────────────────────────────────────

function fixDpiChecksum(packet: Buffer): void {
	let sum = 0;
	for (let i = 3; i <= 49; i++) sum = (sum + (packet[i] ?? 0)) & 0xffff;
	packet[50] = (sum >> 8) & 0xff;
	packet[51] = sum & 0xff;
}

function fixPrefsChecksum(packet: Buffer): void {
	let sum = 0;
	for (let i = 3; i <= 10; i++) sum = (sum + (packet[i] ?? 0)) & 0xffff;
	packet[11] = (sum >> 8) & 0xff;
	packet[12] = sum & 0xff;
}

function fixButtonsChecksum(packet: Buffer): void {
	let sum = 0;
	for (let i = 3; i <= 56; i++) sum = (sum + (packet[i] ?? 0)) & 0xffff;
	packet[57] = (sum >> 8) & 0xff;
	packet[58] = sum & 0xff;
}

// ── Diff helpers ─────────────────────────────────────────────────────────────

interface ByteDiff {
	offset: number;
	before: number;
	after: number;
}

function diffBytes(before: Buffer, after: Buffer): ByteDiff[] {
	const diffs: ByteDiff[] = [];
	for (let i = 0; i < Math.max(before.length, after.length); i++) {
		const b = before[i] ?? 0;
		const a = after[i] ?? 0;
		if (b !== a) diffs.push({ offset: i, before: b, after: a });
	}
	return diffs;
}

function formatDiff(diffs: ByteDiff[]): string {
	if (diffs.length === 0) return '  (no changes)';
	return diffs
		.map(
			(d) =>
				`  [${d.offset.toString().padStart(3)}] 0x${d.before.toString(16).padStart(2, '0')} → 0x${d.after.toString(16).padStart(2, '0')} (${d.before} → ${d.after})`,
		)
		.join('\n');
}

function formatHexDump(buffer: Buffer, highlightOffsets: Set<number>): string {
	const lines: string[] = [];
	for (let row = 0; row < buffer.length; row += 16) {
		const hex: string[] = [];
		const ascii: string[] = [];
		for (let col = 0; col < 16 && row + col < buffer.length; col++) {
			const offset = row + col;
			const byte = buffer[offset] ?? 0;
			const hexStr = byte.toString(16).padStart(2, '0');
			hex.push(highlightOffsets.has(offset) ? `*${hexStr}` : ` ${hexStr}`);
			ascii.push(byte >= 0x20 && byte < 0x7f ? String.fromCharCode(byte) : '.');
		}
		lines.push(`${row.toString(16).padStart(4, '0')}  ${hex.join(' ')}  ${ascii.join('')}`);
	}
	return lines.join('\n');
}

// ── Test runner ──────────────────────────────────────────────────────────────

interface TestResult {
	name: string;
	changedBytes: number[];
	diffs: ByteDiff[];
	bulkHex: string;
	writeVerified: boolean;
}

interface Snapshot {
	timestamp: string;
	label: string;
	bulkHex: string;
	bulkBytes: number[];
	metadataHex?: string;
	dpiHex?: string;
	preferencesHex?: string;
	buttonsHex?: string;
}
function runTest(
	device: HID.HID,
	name: string,
	modify: (original: Buffer) => Buffer,
	restore: Buffer,
	baselineBulk: Buffer,
	verifyReportId: number,
	verifyLen: number,
): TestResult {
	console.log(`\n── ${name} ──`);
	const modified = modify(restore);
	console.log(`  Writing: ${modified.toString('hex').slice(0, 80)}...`);
	writeReport(device, modified);

	// Verify the write took effect by reading the same report back.
	const readback = readReport(device, verifyReportId, verifyLen, targetProfile);
	const writeVerified = readback.equals(modified);
	if (!writeVerified) {
		const rbDiffs = diffBytes(modified, readback);
		console.log(`  ⚠ Write NOT verified! Readback differs in ${rbDiffs.length} byte(s):`);
		for (const d of rbDiffs.slice(0, 10)) {
			console.log(
				`    [${d.offset}] wrote 0x${d.before.toString(16).padStart(2, '0')}, read 0x${d.after.toString(16).padStart(2, '0')}`,
			);
		}
	} else {
		console.log(`  ✓ Write verified (readback matches).`);
	}

	const bulk = readReport(device, BULK_REPORT_ID, BULK_LENGTH);
	const diffs = diffBytes(baselineBulk, bulk);
	console.log(`  0x0A changed bytes: ${diffs.length}`);
	if (diffs.length > 0 && diffs.length <= 32) {
		console.log(formatDiff(diffs));
	}

	// Restore.
	writeReport(device, restore);

	// Verify restoration.
	const bulkAfterRestore = readReport(device, BULK_REPORT_ID, BULK_LENGTH);
	const restoreDiffs = diffBytes(baselineBulk, bulkAfterRestore);
	if (restoreDiffs.length > 0) {
		console.log(`  ⚠ Restore left ${restoreDiffs.length} residual diff(s):`);
		console.log(formatDiff(restoreDiffs));
	}

	return {
		name,
		changedBytes: diffs.map((d) => d.offset),
		diffs,
		bulkHex: bulk.toString('hex'),
		writeVerified,
	};
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
	console.log(`\n=== Bulk profile (0x0A) field-mapping probe ===`);
	console.log(`  Transport: ${transportLabel} (PID 0x${targetPid.toString(16)})`);
	console.log(`  Profile: ${targetProfile}`);

	const device = discover();
	console.log(`  Device found.\n`);

	const allSnapshots: Snapshot[] = [];
	const testResults: TestResult[] = [];

	try {
		// ── Baseline ────────────────────────────────────────────────────
		console.log('── Baseline snapshot ──');
		const metadata = readReport(device, 0x0c, FA61_REPORT_LENGTH.PROFILE_METADATA);
		const dpi = readReport(device, 0x04, FA61_REPORT_LENGTH.DPI, targetProfile);
		const prefs = readReport(device, 0x05, FA61_REPORT_LENGTH.PREFERENCES, targetProfile);
		const buttons = readReport(device, 0x08, FA61_REPORT_LENGTH.BUTTONS, targetProfile);
		const bulk = readReport(device, BULK_REPORT_ID, BULK_LENGTH);

		allSnapshots.push({
			timestamp: new Date().toISOString(),
			label: 'baseline',
			bulkHex: bulk.toString('hex'),
			bulkBytes: [...bulk],
			metadataHex: metadata.toString('hex'),
			dpiHex: dpi.toString('hex'),
			preferencesHex: prefs.toString('hex'),
			buttonsHex: buttons.toString('hex'),
		});

		const meta = decodeProfileMetadata(metadata);
		console.log(`  Metadata: current=${meta.current}, max=${meta.maximum}`);
		console.log(`  DPI: ${dpi.toString('hex').slice(0, 40)}...`);
		console.log(`  Prefs: ${prefs.toString('hex')}`);
		console.log(`  Buttons: ${buttons.toString('hex').slice(0, 40)}...`);
		console.log(`  Bulk 0x0A: ${bulk.toString('hex')}`);

		const baselineBulk = Buffer.from(bulk);

		// ── Test 1: DPI active stage ────────────────────────────────────
		const dpiStageResult = runTest(
			device,
			'DPI active stage (1→2)',
			(src) => {
				const p = Buffer.from(src);
				p[24] = 0x02;
				fixDpiChecksum(p);
				return p;
			},
			dpi,
			baselineBulk,
			0x04,
			FA61_REPORT_LENGTH.DPI,
		);
		// ── Test 2: DPI value (stage 1: 800→1600) ──────────────────────
		const dpiValueResult = runTest(
			device,
			'DPI value (stage 1: 800→1600)',
			(src) => {
				const p = Buffer.from(src);
				// 1600 DPI = raw (1600/50 - 1) = 31 = 0x1f
				p[8] = 0x1f; // low byte of stage 1
				p[16] = 0x00; // high byte of stage 1
				fixDpiChecksum(p);
				return p;
			},
			dpi,
			baselineBulk,
			0x04,
			FA61_REPORT_LENGTH.DPI,
		);
		testResults.push(dpiValueResult);

		// ── Test 3: Light mode ──────────────────────────────────────────
		const originalLightMode = prefs[3] ?? 0;
		const toggleLightMode = originalLightMode === 0x00 ? 0x10 : 0x00;
		const lightModeResult = runTest(
			device,
			`Light mode (0x${originalLightMode.toString(16).padStart(2, '0')}→0x${toggleLightMode.toString(16).padStart(2, '0')})`,
			(src) => {
				const p = Buffer.from(src);
				p[2] = targetProfile;
				p[3] = toggleLightMode;
				fixPrefsChecksum(p);
				return p;
			},
			prefs,
			baselineBulk,
			0x05,
			FA61_REPORT_LENGTH.PREFERENCES,
		);
		testResults.push(lightModeResult);

		// ── Test 4: Debounce ────────────────────────────────────────────
		const originalDebounce = prefs[10] ?? 0;
		const toggleDebounce = originalDebounce === 0x02 ? 0x04 : 0x02; // 4ms ↔ 8ms
		const debounceResult = runTest(
			device,
			`Debounce (0x${originalDebounce.toString(16).padStart(2, '0')}→0x${toggleDebounce.toString(16).padStart(2, '0')})`,
			(src) => {
				const p = Buffer.from(src);
				p[2] = targetProfile;
				p[10] = toggleDebounce;
				fixPrefsChecksum(p);
				return p;
			},
			prefs,
			baselineBulk,
			0x05,
			FA61_REPORT_LENGTH.PREFERENCES,
		);
		testResults.push(debounceResult);

		// ── Test 5: Sleep timer ─────────────────────────────────────────
		const originalSleep = prefs[9] ?? 0;
		const toggleSleep = originalSleep === 0x04 ? 0x08 : 0x04; // 2min ↔ 4min
		const sleepResult = runTest(
			device,
			`Sleep timer (0x${originalSleep.toString(16).padStart(2, '0')}→0x${toggleSleep.toString(16).padStart(2, '0')})`,
			(src) => {
				const p = Buffer.from(src);
				p[2] = targetProfile;
				p[9] = toggleSleep;
				fixPrefsChecksum(p);
				return p;
			},
			prefs,
			baselineBulk,
			0x05,
			FA61_REPORT_LENGTH.PREFERENCES,
		);
		testResults.push(sleepResult);

		// ── Test 6: Button assignment ───────────────────────────────────
		// Flip one byte in slot 4 (forward button, offset 21) to a different value.
		// We don't care about the semantic meaning — we just need a byte change.
		const originalButtonByte = buttons[21] ?? 0;
		const toggleButtonByte = (originalButtonByte + 1) & 0xff;
		const buttonResult = runTest(
			device,
			`Button slot 4 byte (offset 21: 0x${originalButtonByte.toString(16).padStart(2, '0')}→0x${toggleButtonByte.toString(16).padStart(2, '0')})`,
			(src) => {
				const p = Buffer.from(src);
				p[2] = targetProfile;
				p[21] = toggleButtonByte;
				fixButtonsChecksum(p);
				return p;
			},
			buttons,
			baselineBulk,
			0x08,
			FA61_REPORT_LENGTH.BUTTONS,
		);
		testResults.push(buttonResult);

		// ── Test 7: Profile metadata switch ─────────────────────────────
		// Switch current profile via 0x0c and read 0x0a at the NEW profile.
		// This is the most invasive test — it changes persistent metadata.
		const otherProfile = targetProfile === 1 ? 2 : 1;
		console.log(`\n── Profile switch (${targetProfile}→${otherProfile}) ──`);
		console.log(`  Writing 0x0c profile switch...`);

		// Build profile control report.
		const profilePacket = Buffer.from([
			0x0c,
			0x0a,
			otherProfile,
			~otherProfile & 0xff,
			meta.maximum,
			~meta.maximum & 0xff,
		]);
		writeReport(device, profilePacket);

		// Read 0x0a at the OTHER profile.
		const bulkOther = readReport(device, BULK_REPORT_ID, BULK_LENGTH, otherProfile);
		const profileDiffs = diffBytes(baselineBulk, bulkOther);
		console.log(`  0x0A at profile ${otherProfile}: ${bulkOther.toString('hex').slice(0, 80)}...`);
		console.log(`  Changed bytes vs baseline: ${profileDiffs.length}`);
		if (profileDiffs.length > 0 && profileDiffs.length <= 64) {
			console.log(formatDiff(profileDiffs));
		}

		testResults.push({
			name: `Profile switch (${targetProfile}→${otherProfile})`,
			changedBytes: profileDiffs.map((d) => d.offset),
			diffs: profileDiffs,
			bulkHex: bulkOther.toString('hex'),
		});

		allSnapshots.push({
			timestamp: new Date().toISOString(),
			label: `profile-${otherProfile}`,
			bulkHex: bulkOther.toString('hex'),
			bulkBytes: [...bulkOther],
		});

		// Switch back.
		console.log(`  Restoring profile ${targetProfile}...`);
		const restorePacket = Buffer.from([
			0x0c,
			0x0a,
			targetProfile,
			~targetProfile & 0xff,
			meta.maximum,
			~meta.maximum & 0xff,
		]);
		writeReport(device, restorePacket);

		// ── Summary ─────────────────────────────────────────────────────
		console.log('\n\n═══════════════════════════════════════════════════════════════');
		console.log('  SUMMARY: 0x0A field-mapping results');
		console.log('═══════════════════════════════════════════════════════════════\n');

		// Build a per-offset map: which tests touched each byte?
		const offsetMap = new Map<number, string[]>();
		for (const test of testResults) {
			for (const offset of test.changedBytes) {
				const existing = offsetMap.get(offset) ?? [];
				existing.push(test.name);
				offsetMap.set(offset, existing);
			}
		}

		// Print offset map.
		console.log('Offset map (which tests changed each byte):');
		for (const [offset, tests] of [...offsetMap.entries()].sort((a, b) => a[0] - b[0])) {
			console.log(`  [${offset.toString().padStart(3)}] ← ${tests.join(', ')}`);
		}

		// Per-test summary with write verification.
		console.log('\nPer-test summary:');
		for (const test of testResults) {
			const verify = test.writeVerified ? '✓' : '⚠ NOT VERIFIED';
			console.log(`  ${test.name}: ${test.changedBytes.length} byte(s) changed [${verify}]`);
		}
		// Identify clean regions (bytes never changed = static/unknown).
		const allChanged = new Set(offsetMap.keys());
		const staticBytes: number[] = [];
		for (let i = 0; i < BULK_LENGTH; i++) {
			if (!allChanged.has(i)) staticBytes.push(i);
		}
		console.log(`\nStatic bytes (never changed): ${staticBytes.length}/${BULK_LENGTH}`);

		// Identify overlapping regions.
		const overlaps = [...offsetMap.entries()].filter(([, tests]) => tests.length > 1);
		if (overlaps.length > 0) {
			console.log('\nOverlapping bytes (changed by multiple tests):');
			for (const [offset, tests] of overlaps) {
				console.log(`  [${offset}] ← ${tests.join(' + ')}`);
			}
		} else {
			console.log('\nNo overlapping bytes — all test regions are disjoint.');
		}

		// Hex dump with all changed bytes marked.
		console.log('\nBaseline 0x0A with all changed bytes marked (*):');
		console.log(formatHexDump(baselineBulk, allChanged));
	} finally {
		device.close();
		const session = {
			startedAt: new Date().toISOString(),
			targetProfile,
			snapshots: allSnapshots,
			testResults: testResults.map((t) => ({
				name: t.name,
				changedBytes: t.changedBytes,
				diffs: t.diffs,
				bulkHex: t.bulkHex,
				writeVerified: t.writeVerified,
			})),
			completedAt: new Date().toISOString(),
		};
		await Bun.write(outPath, `${JSON.stringify(session, null, 2)}\n`);
		console.log(`\nSession written to ${outPath}`);
	}
}

await main();
