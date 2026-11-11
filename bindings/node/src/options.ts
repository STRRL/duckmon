/**
 * Checks an option that must be a whole number from `least` to `most`.
 * Returns undefined when the option is not set.
 */
export function wholeNumber(
  value: unknown,
  name: string,
  least: number,
  most: number = Number.MAX_SAFE_INTEGER,
): number | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < least || value > most) {
    const range = most === Number.MAX_SAFE_INTEGER ? `of at least ${least}` : `from ${least} to ${most}`;
    throw new TypeError(`${name} must be a whole number ${range}`);
  }
  return value;
}
