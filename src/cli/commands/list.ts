import * as HID from 'node-hid';
import { LIST_HELP } from '../help.js';
import { PID_TO_TRANSPORT_LABEL } from '../types.js';

const VID = 0x1d57;
const TARGET_PIDS: Record<number, true> = { 0xfa60: true, 0xfa61: true };
export const help = LIST_HELP;

export async function run(): Promise<void> {
	const devices = await HID.devicesAsync();
	const matched = devices.filter((d) => d.vendorId === VID && TARGET_PIDS[d.productId] === true);
	if (matched.length === 0) {
		console.log('No Attack Shark X3 devices found.');
		return;
	}

	console.log('TRANSPORT   PRODUCT                     IFACE  PATH');
	console.log('----------  --------------------------  -----  ----');
	for (const d of matched) {
		const transport = PID_TO_TRANSPORT_LABEL[d.productId] ?? `0x${d.productId.toString(16)}`;
		const product = (d.product ?? 'Unknown').padEnd(26);
		const iface = d.interface;
		const path = d.path ?? '-';
		console.log(`${transport.padEnd(10)}  ${product}  ${String(iface).padEnd(5)}  ${path}`);
	}
}
