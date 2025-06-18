import React, { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { db } from "../firebase/firebaseConfig";
import { ref, set, update, onValue } from "firebase/database";
import BackToMenuButton from "./buttonRtnLogin";
import { BeakerIcon, BoltIcon, FireIcon, ShieldExclamationIcon, StarIcon, UserIcon } from "@heroicons/react/16/solid";

const colorOptions = {
  default: "rgba(31, 41, 55, 0.2)", // Cinza escuro
  green: "rgba(34, 197, 94, 0.2)",  // Verde
  blue: "rgba(0, 68, 238, 0.2)",  
  red: "rgba(239, 68, 68, 0.2)",    // Vermelho
  yellow: "rgba(234, 179, 8, 0.2)", // Amarelo
};

export default function CdPage() {
    const [scale, setScale] = useState(1); // Inicialmente a escala é 1 (tamanho original)
    const [gridMode, setGridMode] = useState<"2-cols" | "3-cols" | "4-cols" | "5-cols" | "free">("2-cols");

  const [cooldowns, setCooldowns] = useState({
    bota: false,
    peito: false,
    elmo: false,
    pocao: false,
    ultimate: false,
  });
  const [users, setUsers] = useState<{ [key: string]: { cooldown: { [key: string]: boolean } } }>( {});
  const [orderedUsers, setOrderedUsers] = useState<string[]>([]);
  
  const [cooldownTimes, setCooldownTimes] = useState({
    bota: 60,
    peito: 60,
    elmo: 60,
    pocao: 60,
    ultimate: 60,
  });
  const partyId = typeof window !== "undefined" ? localStorage.getItem("partyId") : null;
  const [tempColors, setTempColors] = useState<{ [key: string]: string }>({});

  const [isSelectVisible, setSelectVisible] = useState(true); // Estado para controlar a visibilidade do select


  // Função para atualizar o cooldown específico no Firebase sem sobrescrever os outros cooldowns
  const updateCooldown = (nickname: string, cooldownName: string, cooldownStatus: boolean) => {
    if (!partyId) {
      console.error("Party ID não encontrado");
      return;
    }

    const userRef = ref(db, `parties/${partyId}/users/${nickname}/cooldown`);
    update(userRef, {
      [cooldownName]: cooldownStatus,
    })
      .then(() => console.log(`${nickname} cooldown de ${cooldownName} atualizado com sucesso`))
      .catch((error) => console.error("Erro ao atualizar cooldown:", error));
  };

  const handleScaleIncrease = () => {
    setScale(prevScale => prevScale * 1.1); // Aumenta a escala em 10%
  };

  const handleScaleDecrease = () => {
    setScale(prevScale => prevScale / 1.1); // Diminui a escala em 10%
  };
  const cooldownTimers = useRef<{ [key: string]: ReturnType<typeof setTimeout> | null }>({
    bota: null,
    peito: null,
    pocao: null,
    elmo: null,
    ultimate: null,
  });

// Função para iniciar o timer do cooldown
const startCooldownTimer = (cooldownName: keyof typeof cooldownTimes, nickname: string) => {
  // Verificar se já existe um timer ativo para o cooldown
  if (cooldowns[cooldownName]) {
    console.log(`${cooldownName} já está em cooldown.`);
    return; // Não iniciar outro timer se já estiver ativo
  }

  // Verificar se existe um timer pendente (referência no useRef)
  if (cooldownTimers.current[cooldownName]) {
    console.log(`Timer de ${cooldownName} já iniciado.`);
    return; // Se já houver um timer em andamento, não cria outro
  }

  const cooldownTime = cooldownTimes[cooldownName] * 1000; // Tempo do cooldown em milissegundos

  // Ativar o cooldown para esse item
  setCooldowns((prevCooldowns) => ({
    ...prevCooldowns,
    [cooldownName]: true,
  }));

  // Atualizar o Firebase para indicar que o cooldown foi ativado
  updateCooldown(nickname, cooldownName, true);

  // Iniciar o timer independente para cada cooldown
  cooldownTimers.current[cooldownName] = setTimeout(() => {
    setCooldowns((prevCooldowns) => ({
      ...prevCooldowns,
      [cooldownName]: false,
    }));

    updateCooldown(nickname, cooldownName, false);

    // Limpar o timer após o cooldown
    cooldownTimers.current[cooldownName] = null;
  }, cooldownTime); // Usa o tempo configurado no login
};
  // Efeito para pegar a lista de usuários e suas informações de cooldown do Firebase
  useEffect(() => {
    if (!partyId) {
      console.error("Party ID não encontrado");
      return;
    }

    const usersRef = ref(db, `parties/${partyId}/users`);
    onValue(usersRef, (snapshot) => {
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
  }, [partyId]);

  // Puxar o tempo de cooldown do login
  useEffect(() => {
    if (typeof window !== "undefined") {
      const storedUltimateCooldown = localStorage.getItem('ultimateCooldown');
      const storedPeitoCooldown = localStorage.getItem('peitoCooldown');
      const storedBotaCooldown = localStorage.getItem('botaCooldown');
      const storedElmoCooldown = localStorage.getItem('elmoCooldown');
      const storedPocaoCooldown = localStorage.getItem('pocaoCooldown');

      if (storedUltimateCooldown) {
        setCooldownTimes((prev) => ({
          ...prev,
          ultimate: parseInt(storedUltimateCooldown),
        }));
      }
      if (storedPeitoCooldown) {
        setCooldownTimes((prev) => ({
          ...prev,
          peito: parseInt(storedPeitoCooldown),
        }));
      }
      if (storedBotaCooldown) {
        setCooldownTimes((prev) => ({
          ...prev,
          bota: parseInt(storedBotaCooldown),
        }));
      }
      if (storedElmoCooldown) {
        setCooldownTimes((prev) => ({
          ...prev,
          elmo: parseInt(storedElmoCooldown),
        }));
      }
      if (storedPocaoCooldown) {
        setCooldownTimes((prev) => ({
          ...prev,
          pocao: parseInt(storedPocaoCooldown),
        }));
      }
    }
  }, []);

 useEffect(() => {
  const handleCooldownEvent = (event: { event: string }) => {
    const nickname = typeof window !== "undefined" ? localStorage.getItem("nickname") : null;
    if (!nickname) return;

    console.log('Evento de cooldown recebido:', event);

    switch (event.event) {
      case "bota":
        startCooldownTimer("bota", nickname);
        break;
      case "peito":
        startCooldownTimer("peito", nickname);
        break;
      case "pocao":
        startCooldownTimer("pocao", nickname);
        break;
      case "elmo":
        startCooldownTimer("elmo", nickname);
        break;
      case "ultimate":
        startCooldownTimer("ultimate", nickname);
        break;
      default:
        break;
    }
  };

  // Registrando ouvintes e armazenando unlisteners
  const unlisteners: Promise<() => void>[] = [
    listen("bota", handleCooldownEvent),
    listen("peito", handleCooldownEvent),
    listen("elmo", handleCooldownEvent),
    listen("pocao", handleCooldownEvent),
    listen("ultimate", handleCooldownEvent),
  ];

  // Cleanup
  return () => {
    unlisteners.forEach(async (unlistenPromise) => {
      const unlisten = await unlistenPromise;
      unlisten(); // Remove o listener
    });
  };
}, [partyId, cooldownTimes]);


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
      const handleEvent = (event: { event: string }) => {
        if (event.event === "esconder") {
          setSelectVisible(false);
        } else if (event.event === "exibir") {
          setSelectVisible(true);
        }
      };
  
      listen("esconder", handleEvent);
      listen("exibir", handleEvent);
  
      return () => {
        listen("esconder", handleEvent).then((unlisten) => unlisten());
        listen("exibir", handleEvent).then((unlisten) => unlisten());
      };
    }, []);

  const toggleGridMode = () => {
    setGridMode((prevMode) => {
      if (prevMode === "2-cols") return "3-cols";
      if (prevMode === "3-cols") return "4-cols";
      if (prevMode === "4-cols") return "5-cols";
      return "2-cols"; // Retorna para 2 colunas após 5
    });
  };

   
    return (
      <div className="flex flex-col items-center">
        {isSelectVisible && (
          <div className="fixed space-x-4 z-50"> {/* Fixando os botões */}
            <BackToMenuButton />
            <button
              onClick={handleScaleIncrease}
              className="px-4 py-2 bg-blue-500 text-white rounded"
            >
              +
            </button>
            <button
              onClick={handleScaleDecrease}
              className="px-4 py-2 bg-blue-500 text-white rounded"
            >
              -
            </button>
            <button
              onClick={toggleGridMode}
              className="px-4 py-2 bg-blue-500 text-white rounded"
            >
              Grid Mode
            </button>
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
                  : "grid-cols-2" // Default fallback para 2 colunas
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
                    <div className="flex flex-col items-center p-2"> {/* Aqui eu adicionei o padding */}
                      <span className="text-xs mb-1">{nickname}</span>
                      <div className="flex gap-2">
                        {Object.keys(cooldowns).map((cooldownName) => {
                          const isCooldownActive = userCooldowns[cooldownName];
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
                              const { [nickname]: _, ...rest } = tempColors; // Remove a cor do estado
                              setTempColors(rest);
                            } else {
                              setTempColors({ ...tempColors, [nickname]: value });
                            }
                          }}
                          value={tempColors[nickname] || "default"} // Mantém sincronizado
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