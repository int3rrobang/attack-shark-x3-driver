import { describe, expect, it } from 'bun:test';
import { TransportKind } from '../src/types.js';
import { PollingRateBuilder, Rate } from '../src/protocols/PollingRateBuilder.js';

describe('PollingRateBuilder', () => {
	it.each([
		[Rate.powerSaving, '06090108f700000000'],
		[Rate.office, '06090104fb00000000'],
		[Rate.gaming, '06090102fd00000000'],
		[Rate.eSports, '06090101fe00000000'],
	])('builds rate %d', (rate, expected) => {
		const buffer = PollingRateBuilder.forRate(rate).build(TransportKind.Wired);
		expect(buffer.toString('hex')).toBe(expected);
	});

	it('accepts the receiver transport contract', () => {
		const builder = new PollingRateBuilder({ rate: Rate.office });
		const buffer = builder.build(TransportKind.Receiver);
		expect(buffer.length).toBe(9);
		expect(buffer[4]).toBe(0xfb);
	});

	it('supports changing the rate', () => {
		const builder = new PollingRateBuilder();
		builder.setRate(Rate.office).build(TransportKind.Wired);
		expect(builder.buffer[3]).toBe(0x04);
		expect(builder.buffer[4]).toBe(0xfb);
	});
});
