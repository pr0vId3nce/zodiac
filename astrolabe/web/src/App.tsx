import { useEffect, useRef, useState, type ReactNode } from "react";
import { Herd } from "./Herd";
import { Pane } from "./Pane";
import { useAstrolabe } from "./ws";
import { HostVitals } from "./HostVitals";
import { isNativeShell } from "./native";
import { cn } from "./ui";
import { useScreenNav, type Screen } from "./Nav";

function useHashRoute(): [number | null, (id: number | null) => void] {
  const parse = () => {
    const m = location.hash.match(/^#\/p\/(\d+)$/);
    return m ? Number(m[1]) : null;
  };
  const [pane, setPane] = useState<number | null>(parse);
  useEffect(() => {
    const onHash = () => setPane(parse());
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const nav = (id: number | null) => {
    if (id === null) {
      // prefer browser-back so the phone's back gesture stays coherent
      if (location.hash) history.back();
      else setPane(null);
    } else {
      location.hash = `#/p/${id}`;
    }
  };
  return [pane, nav];
}

export default function App() {
  const astrolabe = useAstrolabe();
  const [paneId, nav] = useHashRoute();

  const pane = astrolabe.state?.panes.find((p) => p.id === paneId) ?? null;

  // pane vanished (closed on the desktop) → drop back to the herd
  useEffect(() => {
    if (paneId !== null && astrolabe.state && !pane) nav(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paneId, pane, astrolabe.state]);

  const offline = !astrolabe.connected || !astrolabe.link;

  // renderScreen("pane") wants to hand the swipe gesture a way to sync
  // Nav's state without replaying its own transition — but that function
  // (`jump`) only exists once useScreenNav(renderScreen) has already run.
  // A ref breaks the circularity: renderScreen closes over the ref (stable
  // identity), the ref's `.current` is refreshed after useScreenNav below,
  // and by the time a real swipe actually calls it, it's always current.
  const jumpRef = useRef<(s: Screen) => void>(() => {});

  const renderHerd = (): ReactNode => (
    <>
      {/* one quiet header line: station name in accent, connection dot,
          vitals right-aligned — the orrery's instrument label */}
      <header className="safe-top sticky top-0 z-10 flex items-center gap-2.5 bg-sky-deep/70 px-[18px] pb-2.5 pt-3 backdrop-blur">
        <span className="font-mono text-[10px] font-semibold text-gold">
          {astrolabe.session}
        </span>
        <span
          className={cn(
            "inline-block h-1.5 w-1.5 rounded-full",
            !astrolabe.connected
              ? "bg-red-400"
              : !astrolabe.link
                ? "bg-amber-400"
                : "bg-emerald-400"
          )}
          title={
            !astrolabe.connected
              ? "bridge unreachable"
              : !astrolabe.link
                ? "zodiac server down"
                : "connected"
          }
        />
        <div className="ml-auto">
          <HostVitals />
        </div>
      </header>
      {offline && (
        <div className="bg-red-500/10 px-4 py-1.5 text-center text-caption text-red-300">
          {astrolabe.unauthorized
            ? "missing or wrong token — reopen the link your bridge gave you"
            : !astrolabe.connected
              ? "reconnecting to the bridge…"
              : "bridge is up, but no zodiac server is running"}
        </div>
      )}
      <main className="min-h-0 flex-1 overflow-y-auto">
        <Herd state={astrolabe.state} onOpen={(id) => nav(id)} />
      </main>
    </>
  );

  const renderScreen = (screen: Screen): ReactNode => {
    if (screen === "pane") {
      if (!pane || !astrolabe.state) return null;
      return (
        <Pane
          pane={pane}
          state={astrolabe.state}
          watch={astrolabe.watch}
          commands={astrolabe.commands}
          onBack={() => nav(null)}
          // A completed swipe already revealed Herd live behind the Pane —
          // jump Nav straight to "herd" instead of letting it replay its
          // own slide-in on top of a screen that's already sitting there.
          onSwipeComplete={() => {
            nav(null);
            jumpRef.current("herd");
          }}
          backdrop={renderHerd()}
        />
      );
    }
    return renderHerd();
  };

  const screenNav = useScreenNav(pane ? "pane" : "herd", renderScreen);
  jumpRef.current = screenNav.jump;

  return (
    <div className="relative h-full overflow-hidden">
      {screenNav.slots.map((slot) => (
        <div
          key={slot.key}
          data-nav-screen
          className={cn(
            "absolute inset-0 flex h-full flex-col transition-transform duration-300 ease-[var(--ease-ios-sheet)]",
            slot.className
          )}
        >
          {slot.node}
        </div>
      ))}
    </div>
  );
}
