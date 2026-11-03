import React, { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { db } from "../firebase/firebaseConfig";
import { ref, update, onValue } from "firebase/database";
import BackToMenuButton from "./buttonRtnLogin";
import { BeakerIcon, BoltIcon, ShieldExclamationIcon, StarIcon, UserIcon } from "@heroicons/react/16/solid";

const colorOptions = {
  default: "rgba(31, 41, 55, 0.2)", // Cinza escuro
  green: "rgba(34, 197, 94, 0.2)",  // Verde
  blue: "rgba(0, 68, 238, 0.2)",
  red: "rgba(239, 68, 68, 0.2)",    // Vermelho
  yellow: "rgba(234, 179, 8, 0.2)", // Amarelo
};

const COOLDOWN_KEYS = ["bota", "peito", "elmo", "pocao", "ultimate"] as const;

interface CooldownChangePayload {
  changes: { [key: string]: boolean };
}

export default function CdPage() {
  const [scale, setScale] = useState(1);
  const [gridMode, setGridMode] = useState<"2-cols" | "3-cols" | "4-cols" | "5-cols" | "free">("2-cols");
  const [users, setUsers] = useState<{ [key: string]: { cooldown?: { [key: string]: boolean } } }>({});
  const [orderedUsers, setOrderedUsers] = useState<string[]>([]);

  const partyId = typeof window !== "undefined" ? localStorage.getItem("partyId") : null;
  const [tempColors, setTempColors] = useState<{ [key: string]: string }>({});
  const [isSelectVisible, setSelectVisible] = useState(true);

  const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  const handleScaleIncrease = () => {
    setScale((prevScale) => prevScale * 1.1);
  };

  const handleScaleDecrease = () => {
    setScale((prevScale) => prevScale / 1.1);
  };

  const handleReselectZone = async () => {
    if (!isTauri) return;
    try {
      await invoke("open_region_selector");
    } catch (err) {
      console.error("Erro ao abrir seletor:", err);
    }
  };

  // Garante que o monitoramento de Visão Computacional no Rust esteja ativo e sincronize mudanças no Firebase
  useEffect(() => {
    if (!isTauri || !partyId) return;

    invoke("set_tracking_active", { active: true }).catch(console.error);

    let unlistenCv: (() => void) | undefined;
    listen<CooldownChangePayload>("cv-cooldown-change", (event) => {
      const nickname = localStorage.getItem("nickname");
      if (!nickname || !event.payload?.changes) return;

      const changes = event.payload.changes;
      // Atualização otimista local imediata (0ms de latência na própria tela)
      setUsers((prev) => {
        const currentUser = prev[nickname] || { cooldown: {} };
        return {
          ...prev,
          [nickname]: {
            ...currentUser,
            cooldown: {
              ...(currentUser.cooldown || {}),
              ...changes,
            },
          },
        };
      });

      const userCooldownRef = ref(db, `parties/${partyId}/users/${nickname}/cooldown`);
      update(userCooldownRef, changes).catch((err) =>
        console.error("Erro ao enviar cooldown CV para o Firebase:", err)
      );
    }).then((fn) => {
      unlistenCv = fn;
    });

    return () => {
      if (unlistenCv) unlistenCv();
    };
  }, [isTauri, partyId]);

  // Escuta em tempo real a lista de usuários e seus cooldowns no Firebase
  useEffect(() => {
    if (!partyId) {
      console.error("Party ID não encontrado");
      return;
    }

    const usersRef = ref(db, `parties/${partyId}/users`);
    const unsubscribe = onValue(usersRef, (snapshot) => {
      const data = snapshot.val();
      if (data) {
        setUsers(data);

        const savedOrder = typeof window !== "undefined" ? localStorage.getItem("orderedUsers") : null;
        const savedOrderArray = savedOrder ? JSON.parse(savedOrder) : [];

        const newOrderedUsers = [
          ...savedOrderArray.filter((user: string) => data[user]),
          ...Object.keys(data).filter((user) => !savedOrderArray.includes(user)),
        ];

        setOrderedUsers(newOrderedUsers);
        if (typeof window !== "undefined") {
          localStorage.setItem("orderedUsers", JSON.stringify(newOrderedUsers));
        }
      }
    });

    return () => unsubscribe();
  }, [partyId]);

  // Função para mover os usuários para cima ou para baixo
  const moveUser = (index: number, direction: "up" | "down") => {
    const newOrderedUsers = [...orderedUsers];

    if (direction === "up" && index > 0) {
      const [movedUser] = newOrderedUsers.splice(index, 1);
      newOrderedUsers.splice(index - 1, 0, movedUser);
    } else if (direction === "down" && index < newOrderedUsers.length - 1) {
      const [movedUser] = newOrderedUsers.splice(index, 1);
      newOrderedUsers.splice(index + 1, 0, movedUser);
    }

    setOrderedUsers(newOrderedUsers);
    if (typeof window !== "undefined") {
      localStorage.setItem("orderedUsers", JSON.stringify(newOrderedUsers));
    }
  };

  useEffect(() => {
    if (!isTauri) return;

    let unlistenHide: (() => void) | undefined;
    let unlistenShow: (() => void) | undefined;

    listen("esconder", () => setSelectVisible(false)).then((fn) => {
      unlistenHide = fn;
    });
    listen("exibir", () => setSelectVisible(true)).then((fn) => {
      unlistenShow = fn;
    });

    return () => {
      if (unlistenHide) unlistenHide();
      if (unlistenShow) unlistenShow();
    };
  }, [isTauri]);

  const toggleGridMode = () => {
    setGridMode((prevMode) => {
      if (prevMode === "2-cols") return "3-cols";
      if (prevMode === "3-cols") return "4-cols";
      if (prevMode === "4-cols") return "5-cols";
      return "2-cols";
    });
  };

  return (
    <div className="flex flex-col items-center">
      {isSelectVisible && (
        <div className="fixed space-x-3 z-50 flex items-center">
          <BackToMenuButton />
          <button
            onClick={handleScaleIncrease}
            className="px-3 py-2 bg-blue-500 text-white rounded"
          >
            +
          </button>
          <button
            onClick={handleScaleDecrease}
            className="px-3 py-2 bg-blue-500 text-white rounded"
          >
            -
          </button>
          <button
            onClick={toggleGridMode}
            className="px-3 py-2 bg-blue-500 text-white rounded text-sm"
          >
            Grid Mode
          </button>
          {isTauri && (
            <button
              onClick={handleReselectZone}
              className="px-3 py-2 bg-amber-500 hover:bg-amber-400 text-black font-semibold rounded text-sm"
            >
              🎯 Zona
            </button>
          )}
        </div>
      )}

      <div className="mt-24 w-full top-32" style={{ transform: `scale(${scale})` }}>
        <div className="flex flex-wrap justify-start gap-4">
          <div
            className={`grid gap-4 ${
              gridMode === "2-cols"
                ? "grid-cols-2"
                : gridMode === "3-cols"
                ? "grid-cols-3"
                : gridMode === "4-cols"
                ? "grid-cols-4"
                : gridMode === "5-cols"
                ? "grid-cols-5"
                : "grid-cols-2"
            }`}
          >
            {orderedUsers.map((nickname, index) => {
              const userCooldowns = users[nickname]?.cooldown || {};
              const bgColor = tempColors[nickname] || colorOptions.default;
              return (
                <div
                  key={nickname}
                  className="flex items-center p-4 bg-gray-800 text-white rounded opacity-80 w-36 h-24 justify-center flex-grow sm:w-28 md:w-36 lg:w-38"
                  style={{ backgroundColor: bgColor }}
                >
                  <div className="flex flex-col items-center p-2">
                    <span className="text-xs mb-1">{nickname}</span>
                    <div className="flex gap-2">
                      {COOLDOWN_KEYS.map((cooldownName) => {
                        const isCooldownActive = Boolean(userCooldowns[cooldownName]);
                        const icon = (() => {
                          switch (cooldownName) {
                            case "bota":
                              return <BoltIcon className="h-4 w-4" />;
                            case "peito":
                              return <ShieldExclamationIcon className="h-4 w-4" />;
                            case "elmo":
                              return <UserIcon className="h-4 w-4" />;
                            case "pocao":
                              return <BeakerIcon className="h-4 w-4" />;
                            case "ultimate":
                              return <StarIcon className="h-4 w-4" />;
                            default:
                              return null;
                          }
                        })();

                        return (
                          <span
                            key={cooldownName}
                            className={`mr-1 ${
                              isCooldownActive ? "text-red-500" : "text-green-500"
                            }`}
                          >
                            {icon}
                          </span>
                        );
                      })}
                    </div>
                    {isSelectVisible && (
                      <select
                        className="mt-1 text-xs p-0.5 h-6 w-20 rounded-md border border-gray-500 bg-gray-200 text-black appearance-none"
                        onChange={(e) => {
                          const value = e.target.value;
                          if (value === "default") {
                            const { [nickname]: _, ...rest } = tempColors;
                            setTempColors(rest);
                          } else {
                            setTempColors({ ...tempColors, [nickname]: value });
                          }
                        }}
                        value={tempColors[nickname] || "default"}
                      >
                        <option value="default">Padrão</option>
                        <option value={colorOptions.green}>Verde</option>
                        <option value={colorOptions.blue}>Azul</option>
                        <option value={colorOptions.red}>Vermelho</option>
                        <option value={colorOptions.yellow}>Amarelo</option>
                      </select>
                    )}
                  </div>
                  {isSelectVisible && (
                    <div className="flex flex-col ml-2">
                      <button
                        onClick={() => moveUser(index, "up")}
                        className="text-blue-500 hover:text-blue-700 mb-1"
                      >
                        ↑
                      </button>
                      <button
                        onClick={() => moveUser(index, "down")}
                        className="text-blue-500 hover:text-blue-700"
                      >
                        ↓
                      </button>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>
      </div>
    </div>
  );
}
