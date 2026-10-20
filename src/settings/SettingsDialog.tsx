import { useEffect, useId, useRef, useState } from "react";
import type {
  FunctionComponent,
  KeyboardEvent as ReactKeyboardEvent,
  ReactNode,
} from "react";
import { createPortal } from "react-dom";
import {
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  clampFontSize,
  useSettingsStore,
} from "../store/settings";
import type { ResumeMode } from "../store/settings";

type CategoryId = "sessions" | "terminal" | "shortcuts";

const CATEGORIES: { id: CategoryId; label: string }[] = [
  { id: "sessions", label: "Sessions" },
  { id: "terminal", label: "Terminal" },
  { id: "shortcuts", label: "Shortcuts" },
];

const RESUME_OPTIONS: { value: ResumeMode; label: string }[] = [
  { value: "manual", label: "Manual" },
  { value: "auto", label: "Automatic" },
];

function Row({
  label,
  labelId,
  disabled,
  children,
}: {
  label: string;
  labelId: string;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <div
      className={`settings__row${disabled ? " settings__row--disabled" : ""}`}
    >
      <span id={labelId} className="settings__label">
        {label}
      </span>
      {children}
    </div>
  );
}

function Toggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (value: boolean) => void;
}) {
  const labelId = useId();
  return (
    <Row label={label} labelId={labelId}>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-labelledby={labelId}
        className="settings__switch"
        onClick={() => onChange(!checked)}
      >
        <span className="settings__switch-thumb" />
      </button>
    </Row>
  );
}

function SessionsSection() {
  const restore = useSettingsStore(
    (s) => s.settings.sessions.restoreAgentSessions,
  );
  const resumeMode = useSettingsStore((s) => s.settings.sessions.resumeMode);
  const update = useSettingsStore((s) => s.update);
  const resumeLabelId = useId();

  return (
    <>
      <Toggle
        label="Restore agent sessions"
        checked={restore}
        onChange={(value) =>
          update("sessions", { restoreAgentSessions: value })
        }
      />
      <Row label="On launch" labelId={resumeLabelId} disabled={!restore}>
        <div
          role="radiogroup"
          aria-labelledby={resumeLabelId}
          aria-disabled={!restore}
          className="settings__segmented"
        >
          {RESUME_OPTIONS.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={resumeMode === option.value}
              disabled={!restore}
              className="settings__segment"
              onClick={() => update("sessions", { resumeMode: option.value })}
            >
              {option.label}
            </button>
          ))}
        </div>
      </Row>
    </>
  );
}

function TerminalSection() {
  const fontSize = useSettingsStore((s) => s.settings.terminal.fontSize);
  const update = useSettingsStore((s) => s.update);
  const labelId = useId();

  const set = (value: number) =>
    update("terminal", { fontSize: clampFontSize(value) });

  return (
    <Row label="Font size" labelId={labelId}>
      <div
        role="group"
        aria-labelledby={labelId}
        className="settings__stepper"
      >
        <button
          type="button"
          className="settings__step"
          aria-label="Decrease font size"
          disabled={fontSize <= FONT_SIZE_MIN}
          onClick={() => set(fontSize - 1)}
        >
          −
        </button>
        <span className="settings__step-value" aria-live="polite">
          {fontSize}
        </span>
        <button
          type="button"
          className="settings__step"
          aria-label="Increase font size"
          disabled={fontSize >= FONT_SIZE_MAX}
          onClick={() => set(fontSize + 1)}
        >
          +
        </button>
      </div>
    </Row>
  );
}

function ShortcutsSection() {
  const paste = useSettingsStore((s) => s.settings.shortcuts.pasteWithCtrlV);
  const newline = useSettingsStore(
    (s) => s.settings.shortcuts.newlineWithModifiedEnter,
  );
  const update = useSettingsStore((s) => s.update);

  return (
    <>
      <Toggle
        label="Ctrl+V pastes from clipboard"
        checked={paste}
        onChange={(value) => update("shortcuts", { pasteWithCtrlV: value })}
      />
      <Toggle
        label="Ctrl+Enter / Shift+Enter insert a new line"
        checked={newline}
        onChange={(value) =>
          update("shortcuts", { newlineWithModifiedEnter: value })
        }
      />
    </>
  );
}

const SECTIONS: Record<CategoryId, FunctionComponent> = {
  sessions: SessionsSection,
  terminal: TerminalSection,
  shortcuts: ShortcutsSection,
};

export function SettingsDialog({ onClose }: { onClose: () => void }) {
  const [category, setCategory] = useState<CategoryId>("sessions");
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const Section = SECTIONS[category];

  useEffect(() => {
    dialogRef.current
      ?.querySelector<HTMLButtonElement>(".settings__category[aria-current]")
      ?.focus();
  }, []);

  // Capture-phase Esc on window so no other (canvas/terminal) handler sees it.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [onClose]);

  // Keep every other key inside the dialog from bubbling to window handlers.
  const stopKeys = (e: ReactKeyboardEvent) => e.stopPropagation();

  const onCategoryKeyDown = (e: ReactKeyboardEvent, index: number) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const step = e.key === "ArrowDown" ? 1 : CATEGORIES.length - 1;
    const next = CATEGORIES[(index + step) % CATEGORIES.length];
    setCategory(next.id);
    dialogRef.current
      ?.querySelector<HTMLButtonElement>(`[data-category="${next.id}"]`)
      ?.focus();
  };

  return createPortal(
    <div
      className="settings__backdrop nowheel"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      onKeyDown={stopKeys}
      onKeyUp={stopKeys}
      onContextMenu={(e) => e.stopPropagation()}
    >
      <div
        ref={dialogRef}
        className="settings"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
      >
        <header className="settings__header">
          <h2 id={titleId} className="settings__title">
            Settings
          </h2>
          <button
            type="button"
            className="settings__close"
            aria-label="Close"
            title="Close"
            onClick={onClose}
          >
            <svg
              width="14"
              height="14"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              aria-hidden="true"
            >
              <path d="M6 6l12 12M18 6L6 18" />
            </svg>
          </button>
        </header>
        <div className="settings__body">
          <nav className="settings__nav">
            {CATEGORIES.map((c, index) => (
              <button
                key={c.id}
                type="button"
                data-category={c.id}
                className="settings__category"
                aria-current={category === c.id ? "page" : undefined}
                onClick={() => setCategory(c.id)}
                onKeyDown={(e) => onCategoryKeyDown(e, index)}
              >
                {c.label}
              </button>
            ))}
          </nav>
          <section className="settings__content" aria-labelledby={titleId}>
            <Section />
          </section>
        </div>
      </div>
    </div>,
    document.body,
  );
}
