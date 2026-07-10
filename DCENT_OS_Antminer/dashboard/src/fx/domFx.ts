export interface PulseOptions {
  willChange?: string;
  setTimeout?: typeof window.setTimeout;
  clearTimeout?: typeof window.clearTimeout;
}
export function pulseElement(
  element: HTMLElement | null | undefined,
  className: string,
  durationMs: number,
  options: PulseOptions = {},
): () => void {
  if (!element || durationMs <= 0) return () => {};

  const setTimer = options.setTimeout ?? window.setTimeout;
  const clearTimer = options.clearTimeout ?? window.clearTimeout;
  const previousWillChange = element.style.willChange;
  const willChange = options.willChange ?? 'transform, opacity';
  let active = true;

  element.style.willChange = willChange;
  element.classList.add(className);

  const timer = setTimer(() => {
    if (!active) return;
    active = false;
    element.classList.remove(className);
    element.style.willChange = previousWillChange;
  }, durationMs);

  return () => {
    if (!active) return;
    active = false;
    clearTimer(timer);
    element.classList.remove(className);
    element.style.willChange = previousWillChange;
  };
}
