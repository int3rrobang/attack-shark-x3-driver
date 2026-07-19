import { TransportKind } from '../types.js';

export const TRANSPORT_MAP: Record<string, TransportKind> = {
	wired: TransportKind.Wired,
	receiver: TransportKind.Receiver,
};

export const PID_TO_TRANSPORT_LABEL: Record<number, string> = {
	0xfa61: 'wired',
	0xfa60: 'receiver',
};

export function parseTransport(raw: string | undefined): TransportKind {
	if (raw === undefined) return TransportKind.Wired;
	const normalized = raw.toLowerCase();
	const transport = TRANSPORT_MAP[normalized];
	if (transport === undefined) {
		throw new Error(`Unknown transport: ${raw}. Supported: wired, receiver`);
	}
	return transport;
}
