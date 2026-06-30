import { useAppStore } from "@/state";
import RecordingControls from "@/components/RecordingControls";
import StatusBadge from "@/components/StatusBadge";
import LevelMeter from "@/components/LevelMeter";
import TranscriptEditor from "@/components/TranscriptEditor";
import NotePanel from "@/components/NotePanel";

/** Recording view: controls, live status, input-level meter, the live editable
 *  transcript and the SOAP note panel (FR-4..FR-12). */
export default function RecordingView() {
  const recordingState = useAppStore((s) => s.recordingState);
  const paused = useAppStore((s) => s.paused);
  const inputLevel = useAppStore((s) => s.inputLevel);

  const metering = recordingState === "RECORDING" && !paused;

  return (
    <section aria-label="Recording" className="flex flex-1 flex-col gap-6 p-6">
      <div className="flex items-center justify-between">
        <h2 className="text-lg font-semibold text-neutral-100">Recording</h2>
        <StatusBadge state={recordingState} paused={paused} />
      </div>

      <LevelMeter level={inputLevel} active={metering} />

      <RecordingControls />

      <TranscriptEditor />

      <NotePanel />
    </section>
  );
}
