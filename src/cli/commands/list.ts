import * as HID from 'node-hid';
import { LIST_HELP } from '../help.js';
import { PID_TO_MODE_LABEL } from '../types.js';

const VID = 0x1d57;
const TARGET_PIDS = new Set([0xfa60, 0xfa55, 0xfa61]);

export const help = LIST_HELP;

export async function run(): Promise<void> {
	const devices = await HID.devicesAsync();
	const matched = devices.filter((d) => d.vendorId === VID && TARGET_PIDS.has(d.productId));

	if (matched.length === 0) {
		console.log('No Attack Shark X11 devices found.');
		return;
	}

	console.log('MODE        PRODUCT                     IFACE  PATH');
	console.log('----------  --------------------------  -----  ----');
	for (const d of matched) {
		const mode = PID_TO_MODE_LABEL[d.productId] ?? `0x${d.productId.toString(16)}`;
		const product = (d.product ?? 'Unknown').padEnd(26);
		const iface = d.interface;
		const path = d.path ?? '-';
		console.log(`${mode.padEnd(10)}  ${product}  ${String(iface).padEnd(5)}  ${path}`);
	}
}
