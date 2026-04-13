/**
 * Alfred Idle Timer — Tracks user activity and fires a callback on inactivity.
 *
 * Listens for mousemove, keydown, scroll, click, and touchstart events on the
 * document. When no activity is detected for timeoutMs, the callback fires once.
 * Activity resets the timer. Subsequent idle periods re-fire.
 *
 * Usage:
 *   import { startIdleTimer, resetIdleTimer } from "/desktop-shell/app-alfred-idle.js";
 *   startIdleTimer(() => overlay.notify("idle", {}), 300000);
 */

let timer = null;
let callback = null;
let timeoutMs = 300000; // default 5 minutes
let running = false;

const ACTIVITY_EVENTS = ["mousemove", "keydown", "scroll", "click", "touchstart"];

function onActivity() {
  if (!running) return;
  resetIdleTimer();
}

function scheduleTimeout() {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => {
    if (typeof callback === "function") {
      callback();
    }
    // After firing, restart the timer so it fires again on next idle period
    scheduleTimeout();
  }, timeoutMs);
}

/**
 * Start the idle timer. Attaches activity listeners to document.
 * @param {Function} cb — called when idle timeout is reached
 * @param {number} [ms=300000] — idle threshold in milliseconds (default 5 min)
 */
export function startIdleTimer(cb, ms) {
  if (running) stopIdleTimer();

  callback = cb;
  timeoutMs = ms || 300000;
  running = true;

  ACTIVITY_EVENTS.forEach((evt) => {
    document.addEventListener(evt, onActivity, { passive: true });
  });

  scheduleTimeout();
}

/**
 * Reset the idle timer (restart the countdown).
 * Called automatically on user activity, but can also be called manually.
 */
export function resetIdleTimer() {
  if (!running) return;
  scheduleTimeout();
}

/**
 * Stop the idle timer and remove all listeners.
 */
export function stopIdleTimer() {
  running = false;
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  ACTIVITY_EVENTS.forEach((evt) => {
    document.removeEventListener(evt, onActivity);
  });
}
