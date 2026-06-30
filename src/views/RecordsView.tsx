/** Records browser shell. The list/open/delete flow lands in F5. */
export default function RecordsView() {
  return (
    <section aria-label="Records" className="flex flex-1 flex-col p-6">
      <h2 className="text-lg font-semibold text-neutral-100">Records</h2>
      <p className="mt-2 text-sm text-neutral-500">Saved encounters appear here (F5).</p>
    </section>
  );
}
