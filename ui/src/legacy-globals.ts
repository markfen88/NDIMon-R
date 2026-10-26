// The synchronous bundle retains the classic-script API during P7 round 1.
// Accessors keep reassignment visible across sections and to inline consumers.
export function expose<T>(name: string, get: () => T, set?: (value: T) => void): void {
  Object.defineProperty(globalThis, name, { configurable: true, enumerable: true, get, set });
}
