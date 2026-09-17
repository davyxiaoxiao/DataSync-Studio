import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ProgressUpdate } from "./types";

const PROGRESS_EVENT = "sync://progress";

export type { ProgressUpdate };

function newJobId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `job-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/**
 * Subscribes once to the shared progress channel and exposes only the updates
 * belonging to the job started by `begin()`.
 */
export function useJobProgress() {
  const [update, setUpdate] = useState<ProgressUpdate | null>(null);
  const [active, setActive] = useState(false);
  const jobRef = useRef<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void listen<ProgressUpdate>(PROGRESS_EVENT, (event) => {
      if (jobRef.current && event.payload.jobId === jobRef.current) {
        setUpdate(event.payload);
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const begin = useCallback(() => {
    const id = newJobId();
    jobRef.current = id;
    setUpdate(null);
    setActive(true);
    return id;
  }, []);

  /** Stop listening for this job, keeping the last update visible. */
  const end = useCallback(() => {
    jobRef.current = null;
    setActive(false);
  }, []);

  const dismiss = useCallback(() => {
    setUpdate(null);
    setActive(false);
  }, []);

  return { update, active, begin, end, dismiss };
}
