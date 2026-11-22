// The code under test of the fixture. It reads one locked package, so the test proves the offline install.
import ms from "ms";

export function sum(a: number, b: number): number {
  return a + b;
}

export function seconds(text: string): number {
  return ms(text) / 1000;
}
