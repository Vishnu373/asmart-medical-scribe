/**
 * Trial-expired gate (implementation.md §1). Once the compiled-in beta end date
 * has passed, the app renders this instead of anything else — a hard stop. The
 * verdict comes from the backend (`trial_status`), where the date is baked into
 * the binary and compared to the system clock.
 */
export default function ExpiredView({ endDate }: { endDate: string }) {
  return (
    <div className="flex h-screen flex-col items-center justify-center bg-neutral-950 p-8 text-neutral-100">
      <div className="w-full max-w-md text-center">
        <h1 className="text-lg font-semibold">This beta has ended</h1>
        <p className="mt-2 text-sm text-neutral-400">
          The ASmart Medical Scribe evaluation period ended on {endDate}. Thanks for
          testing — please reach out to continue using the app.
        </p>
      </div>
    </div>
  );
}
