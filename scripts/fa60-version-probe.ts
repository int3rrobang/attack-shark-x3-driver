#!/usr/bin/env bun

/**
 * Version-report (0x0B) probe across transports and BLE identity states.
 *
 * Reads the 0x0B version report through the A0 selector/mailbox path and
 * compares raw bytes to identify firmware-identity vs runtime-state fields.
 *
 * Designed for single-device workflow: the script runs one transport phase
 * at a time, prompting the user to plug/unplug between phases.  Multiple
 * phases can be chained in one session with `--phases`.
 *
 * Usage:
 *   # Single transport, single label
 *   bun scripts/fa60-version-probe.ts --phases wired:X3 --out /tmp/x3-wired.json
 *
 *   # Two transports on the same X3 (unplug FA60, plug FA61 between phases)
 *   bun scripts/fa60-version-probe.ts --phases receiver:X3,wired:X3 --out /tmp/x3-both.json
 *
 *   # M600 wired with BLE slot toggle
 *   bun scripts/fa60-version-probe.ts --phases wired:M600 --ble-slot --out /tmp/m600.json
 *
 *   # Three-phase: X3 receiver → X3 wired → M600 wired
 *   bun scripts/fa60-version-probe.ts --phases receiver:X3,wired:X3,wired:M600 --out /tmp/all.json
 */

import { createInterface } from 'node:readline';
import { parseArgs } from 'node:util';
import * as HID from 'node-hid';

// ── Constants ────────────────────────────────────────────────────────────────

const VID = 0x1d57;
const FA61_PID = 0xfa61;
const FA60_PID = 0xfa60;
const COL04_PATTERN = /col04/i;

const VERSION_REPORT_ID = 0x0b;
const VERSION_WIRED_LENGTH = 0x08;
const VERSION_RECEIVER_LENGTH = 0x0a;
const SELECTOR_REPORT_ID = 0xa0;
const READY_REPORT_LENGTH = 0x08;
const READY_OK = 0x01;

const SELECTOR_SETTLE_MS = 750;
const INTER_READ_MS = 2000;
const BLE_RECONNECT_MS = 10_000;
const BLE_POLL_MS = 500;
const DEFAULT_READS = 10;

// ── CLI ──────────────────────────────────────────────────────────────────────

const HELP = `Version-report (0x0B) transport and BLE-identity probe

Reads the 0x0B firmware version through the A0 selector/mailbox path and
compares raw bytes across transports and BLE-identity states.

Options:
  --phases <spec>      Comma-separated phase list.  Each entry is
                       <transport>:<label> where transport = wired | receiver | ble
                       and label = X3 | M600.
                       Example: "receiver:X3,wired:X3" runs two phases, prompting
                       to unplug the FA60 and plug in FA61 between them.
  --reads <n>          Version reads per phase  (default: 10)
  --ble-slot          Prompt to toggle BLE pairing slot between two read phases
                       within the SAME transport/label pair.  Inserts a
                       post-toggle sub-phase after the first phase.
  --help               Show this help

Examples:
  # X3: read via FA60 receiver
  bun scripts/fa60-version-probe.ts --phases receiver:X3 --out /tmp/x3-rx.json

  # X3: both transports (sequential, unplug between)
  bun scripts/fa60-version-probe.ts --phases receiver:X3,wired:X3 --out /tmp/x3-both.json

  # X3: both transports + BLE slot toggle
  bun scripts/fa60-version-probe.ts --phases receiver:X3,wired:X3 --ble-slot --out /tmp/x3-ble-slot.json

  # M600 wired + BLE slot toggle
  bun scripts/fa60-version-probe.ts --phases wired:M600 --ble-slot --out /tmp/m600-ble-slot.json
`;

const parsed = parseArgs({
	allowPositionals: false,
	options: {
		phases: { type: 'string' },
		reads: { type: 'string' },
		'ble-slot': { type: 'boolean' },
		'settle-ms': { type: 'string' },
		'inter-read-ms': { type: 'string' },
		out: { type: 'string' },
		help: { type: 'boolean' },
	},
});

if (parsed.values.help) {
	console.log(HELP);
	process.exit(0);
}

const outPath = parsed.values.out;
if (!outPath) throw new Error('--out <path> is required');

const readCount = (() => {
	const raw = parsed.values.reads;
	if (raw === undefined) return DEFAULT_READS;
	const n = Number.parseInt(raw, 10);
	if (!Number.isInteger(n) || n < 1 || n > 200) throw new Error('--reads must be 1–200');
	return n;
})();

const settleMs = (() => {
	const raw = parsed.values['settle-ms'];
	if (raw === undefined) return SELECTOR_SETTLE_MS;
	const n = Number.parseInt(raw, 10);
	if (!Number.isInteger(n) || n < 100) throw new Error('--settle-ms must be >= 100');
	return n;
})();

const interReadMs = (() => {
	const raw = parsed.values['inter-read-ms'];
	if (raw === undefined) return INTER_READ_MS;
	const n = Number.parseInt(raw, 10);
	if (!Number.isInteger(n) || n < 100) throw new Error('--inter-read-ms must be >= 100');
	return n;
})();

const bleSlotToggle = parsed.values['ble-slot'] === true;

type Transport = 'wired' | 'receiver' | 'ble';
type DeviceLabel = 'X3' | 'M600';

interface PhaseSpec {
	readonly transport: Transport;
	readonly label: DeviceLabel;
	readonly phaseName: string;
}

const phaseSpecs: readonly PhaseSpec[] = (() => {
	const raw = parsed.values.phases;
	if (!raw) throw new Error('--phases is required.  Example: --phases wired:X3');
	const specs: PhaseSpec[] = [];
	for (const entry of raw.split(',')) {
		const trimmed = entry.trim();
		if (!trimmed) continue;
		const [transport, label] = trimmed.split(':') as [string | undefined, string | undefined];
		if (transport !== 'wired' && transport !== 'receiver' && transport !== 'ble') {
			throw new Error(`Invalid transport "${transport}" in "${trimmed}".  Use wired, receiver, or ble.`);
		}
		if (label !== 'X3' && label !== 'M600') {
			throw new Error(`Invalid label "${label}" in "${trimmed}".  Use X3 or M600.`);
		}
		specs.push({ transport, label, phaseName: `${transport}:${label}` });
	}
	if (specs.length === 0) throw new Error('--phases must contain at least one entry');
	return specs;
})();

// ── Session structure ────────────────────────────────────────────────────────

interface VersionRead {
	readonly index: number;
	readonly timestamp: string;
	readonly rawHex: string;
	readonly bytes: readonly number[];
	readonly declaredLength: number;
	readonly currentProfile: number | null;
	readonly versionField: string;
	readonly ok: boolean;
	readonly error?: string;
}

interface PhaseResult {
	readonly phaseName: string;
	readonly transport: Transport;
	readonly deviceLabel: DeviceLabel;
	readonly hidPid: number;
	readonly hidPath: string;
	readonly hidProduct: string | undefined;
	readonly reads: readonly VersionRead[];
	readonly distinctPayloads: readonly string[];
	readonly subPhase?: string;
}

interface SessionResult {
	readonly startedAt: string;
	readonly phases: readonly PhaseResult[];
	readonly completedAt?: string;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function sleep(ms: number): void {
	Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function pidForTransport(t: Transport): number {
	return t === 'receiver' ? FA60_PID : FA61_PID;
}

function readLengthForTransport(t: Transport): number {
	return t === 'receiver' ? VERSION_RECEIVER_LENGTH : VERSION_WIRED_LENGTH;
}

function discoverDevice(transport: Transport): { path: string; product: string | undefined } {
	const targetPid = pidForTransport(transport);
	const candidates = (HID.devices() as HID.Device[]).filter(
		(d) => d.vendorId === VID && d.productId === targetPid && COL04_PATTERN.test(d.path ?? ''),
	);
	if (candidates.length === 0) {
		const name = transport === 'receiver' ? 'FA60 receiver (0xfa60)' : 'FA61 wired (0xfa61)';
		throw new Error(`No ${name} Col04 device found.  Is the mouse connected?`);
	}
	if (candidates.length > 1) {
		const paths = candidates.map((c) => c.path).join(', ');
		throw new Error(`Multiple Col04 devices found: ${paths}.  Unplug extras.`);
	}
	const c = candidates[0]!;
	if (!c.path) throw new Error('Device has no HID path');
	return { path: c.path, product: c.product };
}

function readVersionOnce(device: HID.HID, expectedLength: number): { raw: Buffer; declaredLength: number } {
	const selector = Buffer.from([
		SELECTOR_REPORT_ID,
		VERSION_REPORT_ID,
		expectedLength,
		0x00,
		0x01, // target profile 1 (version is untargeted but firmware needs a value)
		0x00,
		0x00,
		0x00,
	]);
	device.sendFeatureReport(selector);
	sleep(settleMs);

	const ready = device.getFeatureReport(SELECTOR_REPORT_ID, READY_REPORT_LENGTH);
	if (ready.length < 2 || ready[0] !== SELECTOR_REPORT_ID || ready[1] !== READY_OK) {
		throw new Error(`Readiness check failed: ${Buffer.from(ready).toString('hex')}`);
	}

	const report = device.getFeatureReport(VERSION_REPORT_ID, expectedLength);
	const raw = Buffer.from(report);
	if (raw.length < 2) throw new Error(`Version report too short: ${raw.length} bytes`);
	return { raw, declaredLength: raw[1] ?? 0 };
}

function parseRead(index: number, raw: Buffer, declaredLength: number): VersionRead {
	const bytes = [...raw];
	return {
		index,
		timestamp: new Date().toISOString(),
		rawHex: raw.toString('hex'),
		bytes,
		declaredLength,
		currentProfile: raw.length > 2 ? raw[2] : null,
		versionField: raw.slice(2).toString('hex'),
		ok: raw[0] === VERSION_REPORT_ID,
	};
}

// ── Phase runner ─────────────────────────────────────────────────────────────

function runPhase(spec: PhaseSpec, subPhase?: string): PhaseResult {
	const displayName = subPhase ? `${spec.phaseName} (${subPhase})` : spec.phaseName;
	console.log(`\n── Phase: ${displayName} ──`);

	const devInfo = discoverDevice(spec.transport);
	const pidName = spec.transport === 'receiver' ? 'FA60 receiver' : 'FA61 wired';
	console.log(`  Device: ${devInfo.product ?? devInfo.path}`);
	console.log(`  PID: 0x${pidForTransport(spec.transport).toString(16)} (${pidName})`);
	console.log(`  Reads: ${readCount}, settle: ${settleMs}ms, gap: ${interReadMs}ms`);

	const device = new HID.HID(devInfo.path);
	const reads: VersionRead[] = [];
	const expectedLength = readLengthForTransport(spec.transport);

	try {
		for (let i = 0; i < readCount; i++) {
			try {
				const { raw, declaredLength } = readVersionOnce(device, expectedLength);
				reads.push(parseRead(i, raw, declaredLength));
				process.stdout.write(`  [${i + 1}/${readCount}] ${raw.toString('hex')}\n`);
			} catch (err) {
				const msg = err instanceof Error ? err.message : String(err);
				reads.push({
					index: i,
					timestamp: new Date().toISOString(),
					rawHex: '',
					bytes: [],
					declaredLength: 0,
					currentProfile: null,
					versionField: '',
					ok: false,
					error: msg,
				});
				process.stdout.write(`  [${i + 1}/${readCount}] ERROR: ${msg}\n`);
			}
			if (i < readCount - 1) sleep(interReadMs);
		}
	} finally {
		device.close();
	}

	const distinct = [...new Set(reads.filter((r) => r.ok).map((r) => r.rawHex))];
	const okCount = reads.filter((r) => r.ok).length;
	console.log(`  Result: ${okCount}/${readCount} OK, ${distinct.length} distinct payload(s)`);
	for (const hex of distinct) console.log(`    ${hex}`);

	return {
		phaseName: spec.phaseName,
		transport: spec.transport,
		deviceLabel: spec.label,
		hidPid: pidForTransport(spec.transport),
		hidPath: devInfo.path,
		hidProduct: devInfo.product,
		reads,
		distinctPayloads: distinct,
		subPhase,
	};
}

// ── Device-switch prompt ─────────────────────────────────────────────────────

function needsDeviceSwitch(prev: PhaseSpec, next: PhaseSpec): boolean {
	return prev.transport !== next.transport;
}

function switchPrompt(from: PhaseSpec, to: PhaseSpec): void {
	const fromName = from.transport === 'receiver' ? 'FA60 receiver' : 'FA61 wired';
	const toName = to.transport === 'receiver' ? 'FA60 receiver' : 'FA61 wired';
	console.log(`\n  ⚠  Device switch needed:`);
	console.log(`     Unplug the ${fromName} and plug in the ${toName}.`);
}

// ── Main ─────────────────────────────────────────────────────────────────────

async function main(): Promise<void> {
	const terminal = createInterface({ input: process.stdin, output: process.stdout });
	const ask = (q: string) => new Promise<string>((r) => terminal.question(`${q}\n`, r));

	const allPhases: PhaseResult[] = [];

	try {
		console.log(`\n=== Version (0x0B) probe ===`);
		console.log(`  Phases: ${phaseSpecs.map((p) => p.phaseName).join(' → ')}`);
		console.log(`  Reads per phase: ${readCount}`);
		console.log(`  Settle: ${settleMs}ms, inter-read: ${interReadMs}ms`);
		if (bleSlotToggle) console.log(`  BLE slot toggle: enabled (All phases get a post-toggle sub-phase)`);

		for (let i = 0; i < phaseSpecs.length; i++) {
			const spec = phaseSpecs[i]!;

			// Prompt for device switch if the transport changes.
			if (i > 0) {
				const prev = phaseSpecs[i - 1]!;
				if (needsDeviceSwitch(prev, spec)) {
					switchPrompt(prev, spec);
					await ask('  Press Enter when the new device is plugged in...');
					// Brief settle for USB enumeration.
					sleep(2000);
				} else if (spec.label !== prev.label) {
					// Same transport, different mouse — still need a swap.
					console.log(`\n  ⚠  Mouse switch: unplug the ${prev.label} and plug in the ${spec.label}.`);
					await ask('  Press Enter when ready...');
					sleep(2000);
				}
			}

			// Run the baseline phase.
			const baseline = runPhase(spec);
			allPhases.push(baseline);

			// BLE slot toggle sub-phase.
			if (bleSlotToggle) {
				console.log(`\n── BLE slot toggle ──`);
				console.log(
					`  Short-press the pairing button on the mouse to switch BLE pairing slots (short-press the pairing button).`,
				);
				console.log(`  The mouse should reconnect automatically.`);
				await ask('  Press Enter after toggling...');
				sleep(BLE_RECONNECT_MS);

				const postToggle = runPhase(spec, 'post-ble-slot');
				allPhases.push(postToggle);

				// Inline comparison.
				const before = baseline.distinctPayloads[0];
				const after = postToggle.distinctPayloads[0];
				if (before && after) {
					if (before === after) {
						console.log(`  BLE slot toggle: NO change in version bytes.`);
					} else {
						console.log(`  BLE slot toggle: version bytes CHANGED:`);
						console.log(`    Before: ${before}`);
						console.log(`    After:  ${after}`);
						const b1 = Buffer.from(before, 'hex');
						const b2 = Buffer.from(after, 'hex');
						for (let j = 0; j < Math.max(b1.length, b2.length); j++) {
							const v1 = b1[j] ?? 0;
							const v2 = b2[j] ?? 0;
							if (v1 !== v2) {
								console.log(
									`    Byte [${j}]: 0x${v1.toString(16).padStart(2, '0')} → 0x${v2.toString(16).padStart(2, '0')}`,
								);
							}
						}
					}
				}
			}
		}

		// ── Cross-phase summary ────────────────────────────────────────────
		if (allPhases.length > 1) {
			console.log(`\n── Cross-phase comparison ──`);
			for (const phase of allPhases) {
				const first = phase.distinctPayloads[0] ?? '(no successful reads)';
				console.log(`  ${phase.phaseName}${phase.subPhase ? ` (${phase.subPhase})` : ''}: ${first}`);
			}

			// Byte-by-byte diff across all phases.
			const allHex = allPhases.map((p) => p.distinctPayloads[0]).filter(Boolean) as string[];
			if (allHex.length >= 2) {
				const bufs = allHex.map((h) => Buffer.from(h, 'hex'));
				const maxLen = Math.max(...bufs.map((b) => b.length));
				console.log(`\n  Byte-by-byte:`);
				for (let j = 0; j < maxLen; j++) {
					const vals = bufs.map((b) => b[j] ?? 0);
					const allSame = vals.every((v) => v === vals[0]);
					const marker = allSame ? '  ' : ' *';
					const hexVals = vals.map((v) => `0x${v.toString(16).padStart(2, '0')}`);
					console.log(`    [${j}]${marker} ${hexVals.join('  ')}`);
				}
				console.log(`  (* = differs across phases)`);
			}
		}
	} catch (err) {
		console.error(`\nFATAL: ${err instanceof Error ? err.message : err}`);
		process.exitCode = 1;
	} finally {
		terminal.close();
		const session: SessionResult = {
			startedAt: new Date().toISOString(),
			phases: allPhases,
			completedAt: new Date().toISOString(),
		};
		await Bun.write(outPath, `${JSON.stringify(session, null, 2)}\n`);
		console.log(`\nSession written to ${outPath}`);
	}
}

await main();
