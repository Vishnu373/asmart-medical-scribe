/** Recording view shell. Controls, level meter and live transcript land in F2/F3. */
export default function RecordingView() {
  return (
    <section aria-label="Recording" className="flex flex-1 flex-col p-6">
      <h2 className="text-lg font-semibold text-neutral-100">Recording</h2>
      <p className="mt-2 text-sm text-neutral-500">
        Record controls and the live transcript appear here (F2–F3).
      </p>
    </section>
  );
}
