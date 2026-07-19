import { describe, expect, it } from 'bun:test';
import { InternalStateResetReportBuilder } from '../src/protocols/InternalStateResetReportBuilder.js';
import { TransportKind } from '../src/types.js';

describe('InternalStateResetReportBuilder', () => {
	it('initializes the confirmed reset payload', () => {
		const builder = new InternalStateResetReportBuilder();
		expect(builder.buffer.toString('hex')).toBe('0c0a01fe01fe00000000');
		expect(builder.calculateChecksum()).toBe(0);
	});

	it('uses the compact wired packet length', () => {
		const buffer = new InternalStateResetReportBuilder().build(TransportKind.Wired);
		expect(buffer.length).toBe(6);
		expect(buffer.toString('hex')).toBe('0c0a01fe01fe');
	});

	it('uses the padded receiver packet length', () => {
		const buffer = new InternalStateResetReportBuilder().build(TransportKind.Receiver);
		expect(buffer.length).toBe(10);
		expect(buffer.toString('hex')).toBe('0c0a01fe01fe00000000');
	});
});
