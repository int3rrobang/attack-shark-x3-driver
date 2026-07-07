import { ConnectionMode } from '../types.js';

export const MODE_MAP: Record<string, ConnectionMode> = {
	'x3-wired': ConnectionMode.X3Wired,
	x3: ConnectionMode.X3Wired,
	wired: ConnectionMode.Wired,
	adapter: ConnectionMode.Adapter,
};

export const PID_TO_MODE_LABEL: Record<number, string> = {
	[ConnectionMode.X3Wired]: 'x3-wired',
	[ConnectionMode.Wired]: 'wired',
	[ConnectionMode.Adapter]: 'adapter',
};

export function parseMode(raw: string | undefined): ConnectionMode {
	if (raw === undefined) return ConnectionMode.X3Wired;
	const normalized = raw.toLowerCase();
	const mode = MODE_MAP[normalized];
	if (mode === undefined) {
		throw new Error(`Unknown mode: ${raw}. Supported: x3-wired, x3, wired, adapter`);
	}
	return mode;
}
