"use client";
import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { db } from "../firebase/firebaseConfig";
import { ref, update } from "firebase/database";

interface RegionBBox {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

interface SlotPreview {
  key: string;
  field: string;
  label: string;
  active: boolean;
  seconds: number | null;
  image_data_url: string;
}

interface VisionStateSnapshot {
  bbox: RegionBBox | null;
  is_tracking: boolean;
  slots: SlotPreview[];
}

const DEFAULT_SLOTS = [
  { key: "E", label: "Ult (E)" },
  { key: "R", label: "Peito (R)" },
  { key: "D", label: "Elmo (D)" },
  { key: "F", label: "Bota (F)" },
  { key: "1", label: "Poção (1)" },
];

export default function Login() {
  const [nickname, setNickname] = useState("");
  const [partyId, setPartyId] = useState("");
  const [bbox, setBbox] = useState<RegionBBox | null>(null);
  const [slots, setSlots] = useState<SlotPreview[]>([]);
  const router = useRouter();

  const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  useEffect(() => {
    if (typeof window !== "undefined") {
      const savedNick = localStorage.getItem("nickname") || "";
      const savedParty = localStorage.getItem("partyId") || "";
      setNickname(savedNick);
      setPartyId(savedParty);
    }

    if (!isTauri) return;

    // Pausa o loop ativo enquanto está no menu inicial e carrega preview salvo
    invoke("set_tracking_active", { active: false }).catch(() => {});
    invoke<VisionStateSnapshot>("get_vision_state")
      .then((snap) => {
        setBbox(snap.bbox);
        setSlots(snap.slots || []);
      })
      .catch(console.error);

    let unlistenRegion: (() => void) | undefined;
    listen<VisionStateSnapshot>("cv-region-updated", (ev) => {
      setBbox(ev.payload.bbox);
      setSlots(ev.payload.slots || []);
    }).then((fn) => {
      unlistenRegion = fn;
    });

    return () => {
      if (unlistenRegion) unlistenRegion();
    };
  }, [isTauri]);

  const handleSelectRegion = async () => {
    if (!isTauri) {
      alert("A seleção de tela funciona dentro do executável Easy CD.");
      return;
    }
    try {
      await invoke("open_region_selector");
    } catch (err) {
      console.error("Erro ao abrir seletor:", err);
    }
  };

  const handleRefreshPreview = async () => {
    if (!isTauri) return;
    try {
      const snap = await invoke<VisionStateSnapshot>("get_vision_state");
      setBbox(snap.bbox);
      setSlots(snap.slots || []);
    } catch (err) {
      console.error(err);
    }
  };

  const handleLogin = async () => {
    if (!nickname.trim()) {
      alert("Digite um nome de usuário válido");
      return;
    }
    if (!partyId.trim() || partyId.length > 10) {
      alert("Digite um ID de Party válido (máx. 10 caracteres)");
      return;
    }
    if (isTauri && !bbox) {
      alert("Selecione primeiro a zona da barra de skills (do E até o 1) na tela!");
      return;
    }

    const cleanNick = nickname.trim();
    const cleanParty = partyId.trim();

    localStorage.setItem("nickname", cleanNick);
    localStorage.setItem("partyId", cleanParty);

    const userRef = ref(db, `parties/${cleanParty}/users/${cleanNick}`);
    try {
      await update(userRef, {
        nickname: cleanNick,
        cooldown: {
          bota: false,
          peito: false,
          elmo: false,
          pocao: false,
          ultimate: false,
        },
      });

      if (isTauri) {
        await invoke("set_tracking_active", { active: true });
      }
      router.push("/cd");
    } catch (error: unknown) {
      const msg = error instanceof Error ? error.message : String(error);
      alert("Erro ao registrar usuário: " + msg);
    }
  };

  return (
    <div className="flex justify-center items-center min-h-screen">
      <div className="bg-gray-900 border border-gray-700 text-white p-6 rounded-xl shadow-2xl w-[440px]">
        <h2 className="text-center text-xl font-bold text-amber-400 mb-1">
          EASY CD • ALBION VISION
        </h2>
        <p className="text-center text-xs text-gray-400 mb-5">
          Leitura automática de cooldowns em tempo real
        </p>

        <div className="space-y-3 mb-4">
          <input
            type="text"
            className="w-full p-2.5 bg-gray-800 border border-gray-700 rounded-lg text-white placeholder-gray-400 focus:outline-none focus:border-amber-400"
            value={nickname}
            onChange={(e) => setNickname(e.target.value)}
            placeholder="Seu Nickname"
          />
          <input
            type="text"
            className="w-full p-2.5 bg-gray-800 border border-gray-700 rounded-lg text-white placeholder-gray-400 focus:outline-none focus:border-amber-400"
            value={partyId}
            onChange={(e) => setPartyId(e.target.value)}
            placeholder="ID PARTY (máx. 10 caracteres)"
            maxLength={10}
          />
        </div>

        {/* Seleção e Preview dos 5 Slots */}
        <div className="bg-gray-800/80 border border-gray-700 rounded-lg p-3 mb-5">
          <div className="flex items-center justify-between mb-2">
            <span className="text-xs font-semibold text-gray-200">
              Barra de Skills (E • R • D • F • 1)
            </span>
            <span
              className={`text-[11px] font-bold ${
                bbox ? "text-green-400" : "text-red-400"
              }`}
            >
              {bbox ? "✓ Zona salva" : "Não selecionada"}
            </span>
          </div>

          <div className="flex gap-2 mb-3">
            <button
              type="button"
              onClick={handleSelectRegion}
              className="flex-1 py-2 px-3 bg-amber-500 hover:bg-amber-400 text-black font-bold text-xs rounded-md transition"
            >
              🎯 Selecionar Barra na Tela
            </button>
            {bbox && (
              <button
                type="button"
                onClick={handleRefreshPreview}
                title="Atualizar foto do preview"
                className="py-2 px-2.5 bg-gray-700 hover:bg-gray-600 text-white text-xs rounded-md transition"
              >
                ↻
              </button>
            )}
          </div>

          {/* 5 Miniaturas de Preview */}
          <div className="grid grid-cols-5 gap-1.5">
            {DEFAULT_SLOTS.map((defSlot, idx) => {
              const slotData = slots[idx];
              const isCd = slotData?.active ?? false;
              return (
                <div key={defSlot.key} className="flex flex-col items-center">
                  <span className="text-[10px] text-gray-300 font-semibold mb-1">
                    {defSlot.label}
                  </span>
                  <div
                    className={`w-14 h-14 rounded bg-gray-900 border-2 overflow-hidden flex items-center justify-center ${
                      slotData
                        ? isCd
                          ? "border-red-500"
                          : "border-green-500"
                        : "border-gray-700"
                    }`}
                  >
                    {slotData?.image_data_url ? (
                      <img
                        src={slotData.image_data_url}
                        alt={defSlot.label}
                        className="w-full h-full object-cover"
                      />
                    ) : (
                      <span className="text-[10px] text-gray-600">{defSlot.key}</span>
                    )}
                  </div>
                  <span
                    className={`text-[9px] font-bold mt-1 ${
                      !slotData
                        ? "text-gray-500"
                        : isCd
                        ? "text-red-400"
                        : "text-green-400"
                    }`}
                  >
                    {!slotData
                      ? "---"
                      : isCd
                      ? `CD (${slotData.seconds ?? ""}s)`
                      : "PRONTO"}
                  </span>
                </div>
              );
            })}
          </div>
        </div>

        <button
          onClick={handleLogin}
          className="w-full py-2.5 bg-blue-600 hover:bg-blue-500 text-white font-bold rounded-lg transition"
        >
          Entrar e Iniciar Monitoramento
        </button>
        <p className="text-center text-[11px] text-gray-400 mt-3">
          Dica: dentro do overlay pressione <b>F2</b> para travar/clicar através no Albion
        </p>
      </div>
    </div>
  );
}
