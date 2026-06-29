import { useEffect, useState } from "react";
import { ping } from "@/bridge";

function App() {
  const [pong, setPong] = useState<string>("…");

  useEffect(() => {
    ping("hello from frontend")
      .then(setPong)
      .catch((e) => setPong(`bridge error: ${e}`));
  }, []);

  return (
    <main className="flex h-screen flex-col items-center justify-center gap-2 bg-neutral-950 text-neutral-100">
      <h1 className="text-2xl font-semibold">Medical Scribe</h1>
      <p className="text-sm text-neutral-400">bridge: {pong}</p>
    </main>
  );
}

export default App;
