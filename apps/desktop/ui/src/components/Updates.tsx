import { Download, RefreshCw } from "lucide-react";
import { useId, useRef, useState } from "react";
import type { Api } from "../ipc/commands";
import { errorCode, errorMessage, type UpdateCheck, type UpdateMode, type UpdateProgress } from "../ipc/types";
import { ConfirmDialog } from "./ConfirmDialog";
import { formatBytes } from "./LocalModels";
import { Button, ICON, InlineMessage } from "./primitives";

type Available = Extract<UpdateCheck, { status: "available" }>;

type State =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "up_to_date"; current: string; at: Date }
  | { kind: "available"; update: Available; error: string | null }
  | { kind: "installing"; update: Available; progress: UpdateProgress | null }
  | { kind: "error"; offline: boolean; message: string };

/** What the row says for builds that don't update themselves. */
const MANAGED: Record<Exclude<UpdateMode, "self_update">, string> = {
  microsoft_store: "The Microsoft Store keeps this copy of Demios up to date.",
  flathub: "Flathub keeps this copy of Demios up to date. Update it from your software centre, or with flatpak update.",
  no_key: "This build has no update key, so it can't check for updates. Get new versions from the Demios releases page on GitHub.",
  off: "This build doesn't update itself. Get new versions from the Demios releases page on GitHub.",
};

const OFFLINE = "You're offline. Connect to the internet, then check again.";

function formatDate(iso: string | null): string | null {
  if (iso === null) return null;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d.toLocaleDateString(undefined, { dateStyle: "medium" });
}

/**
 * Settings › About › Updates (T-STORE-08). Direct downloads check GitHub Releases only when
 * the user asks; a newer version shows its notes, and "Install and restart" asks first.
 * Nothing downloads in the background. Store and Flathub builds say who updates them.
 */
export function Updates({ api, mode, online = () => navigator.onLine }: { api: Api; mode: UpdateMode; online?: () => boolean }) {
  const [state, setState] = useState<State>({ kind: "idle" });
  const [confirming, setConfirming] = useState(false);
  const installRef = useRef<HTMLButtonElement>(null);
  const titleId = useId();
  const notesId = useId();

  if (mode !== "self_update") {
    return (
      <div className="row" data-testid="updates">
        <div className="row-main">
          <span className="row-title">Updates</span>
          <span className="dn-help">{MANAGED[mode]}</span>
        </div>
      </div>
    );
  }

  const check = async () => {
    if (!online()) {
      setState({ kind: "error", offline: true, message: OFFLINE });
      return;
    }
    setState({ kind: "checking" });
    try {
      const r = await api.checkForUpdate();
      setState(
        r.status === "available"
          ? { kind: "available", update: r, error: null }
          : { kind: "up_to_date", current: r.current, at: new Date() },
      );
    } catch (e) {
      const offline = errorCode(e) === "update_offline" || !online();
      setState({ kind: "error", offline, message: offline ? OFFLINE : errorMessage(e) });
    }
  };

  const install = async (update: Available) => {
    setConfirming(false);
    if (!online()) {
      setState({ kind: "available", update, error: OFFLINE });
      return;
    }
    setState({ kind: "installing", update, progress: null });
    try {
      // On success the app restarts, so this only returns on failure.
      await api.installUpdate(update.version, (progress) =>
        setState((s) => (s.kind === "installing" ? { ...s, progress } : s)),
      );
    } catch (e) {
      if (errorCode(e) === "update_stale") {
        setState({ kind: "error", offline: false, message: errorMessage(e) });
      } else {
        setState({ kind: "available", update, error: errorCode(e) === "update_offline" ? OFFLINE : errorMessage(e) });
      }
    }
  };

  const update = state.kind === "available" || state.kind === "installing" ? state.update : null;
  const date = formatDate(update?.date ?? null);

  return (
    <div className="row update-row" data-testid="updates" data-state={state.kind}>
      <div className="row-main">
        <span className="row-title" id={titleId}>
          Updates
        </span>
        <span className="dn-help">
          Checks GitHub for a newer version only when you ask. Nothing downloads until you choose to install it.
        </span>
        <div role="status" aria-live="polite" className="update-status">
          {state.kind === "checking" && <span className="dn-help">Checking for updates…</span>}
          {state.kind === "up_to_date" && (
            <InlineMessage tone="success">
              Demios {state.current} is the latest version. Checked at{" "}
              {state.at.toLocaleTimeString(undefined, { timeStyle: "short" })}.
            </InlineMessage>
          )}
          {update !== null && (
            <span className="update-found">
              Version {update.version} is available. You have {update.current}.{date !== null && ` Released ${date}.`}
            </span>
          )}
          {state.kind === "installing" && <InstallProgress progress={state.progress} labelledBy={titleId} />}
        </div>
        {state.kind === "error" && (
          <InlineMessage tone={state.offline ? "warning" : "error"}>
            {state.offline ? state.message : `Couldn't check for updates. ${state.message}`}
          </InlineMessage>
        )}
        {state.kind === "available" && state.error !== null && (
          <InlineMessage tone="error">Couldn't install the update. {state.error}</InlineMessage>
        )}
        {update !== null && (
          <>
            <span className="dn-help" id={notesId}>
              What's new in {update.version}
            </span>
            {/* Plain text from latest.json: never rendered as Markdown or HTML. */}
            <div className="update-notes" role="region" aria-labelledby={notesId} tabIndex={0} data-testid="update-notes">
              {update.notes === "" ? "No release notes." : update.notes}
            </div>
          </>
        )}
      </div>
      <div className="row-actions">
        {update !== null ? (
          <Button
            ref={installRef}
            variant="primary"
            icon={<Download {...ICON} aria-hidden="true" />}
            loading={state.kind === "installing"}
            disabled={state.kind === "installing"}
            onClick={() => setConfirming(true)}
          >
            Install and restart
          </Button>
        ) : (
          <Button
            icon={<RefreshCw {...ICON} aria-hidden="true" />}
            loading={state.kind === "checking"}
            disabled={state.kind === "checking"}
            onClick={() => void check()}
          >
            {state.kind === "error" ? "Try again" : "Check for updates"}
          </Button>
        )}
      </div>
      <ConfirmDialog
        open={confirming && update !== null}
        onOpenChange={setConfirming}
        title={`Install Demios ${update?.version ?? ""} and restart?`}
        confirmLabel="Install and restart"
        cancelLabel="Not now"
        confirmVariant="primary"
        onConfirm={() => {
          if (update !== null) void install(update);
        }}
        returnFocusTo={installRef}
      >
        <p>
          Demios downloads version {update?.version} from GitHub, checks that it is signed by Demios, installs it and
          restarts. If the signature doesn't match, nothing is installed.
        </p>
        <p>An answer that is still being written stops. Your chats, settings and models stay as they are.</p>
      </ConfirmDialog>
    </div>
  );
}

function InstallProgress({ progress, labelledBy }: { progress: UpdateProgress | null; labelledBy: string }) {
  if (progress?.kind === "installing") {
    return <span className="dn-help">Installing… Demios restarts when it's done.</span>;
  }
  const done = progress?.downloaded ?? 0;
  const total = progress?.total ?? null;
  const label =
    total !== null && total > 0 ? `Downloading ${formatBytes(done)} of ${formatBytes(total)}` : "Downloading the update…";
  return (
    <div className="dl-progress">
      <div
        className="dl-bar"
        role="progressbar"
        aria-labelledby={labelledBy}
        aria-valuemin={0}
        {...(total !== null && total > 0 ? { "aria-valuemax": total, "aria-valuenow": done } : {})}
        aria-valuetext={label}
      >
        <span style={{ width: `${total !== null && total > 0 ? Math.min(100, (done / total) * 100) : 0}%` }} />
      </div>
      <span className="dn-help">{label}</span>
    </div>
  );
}
