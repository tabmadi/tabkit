import { expect, test } from "bun:test";
import { seconds, sum } from "./sum";

test("sum adds two numbers", () => {
  expect(sum(2, 3)).toBe(5);
});

test("seconds reads a duration through the locked package", () => {
  expect(seconds("2m")).toBe(120);
});
