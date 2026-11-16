// Interface de Ricochet. Les données reçues ne passent jamais par innerHTML : seulement
// textContent, pour qu'un nom de machine ne puisse pas injecter de code.
"use strict";

const PERIODE_MS = 10000;
const $ = (selecteur) => document.querySelector(selecteur);
let minuterie = null;

class ErreurApi extends Error {
  constructor(statut, texte) {
    super(texte || `erreur ${statut}`);
    this.statut = statut;
  }
}

async function api(methode, chemin, corps) {
  const options = {
    method: methode,
    credentials: "same-origin",
    headers: { "X-Ricochet": "1" },
  };
  if (corps !== undefined) {
    options.headers["Content-Type"] = "application/json";
    options.body = JSON.stringify(corps);
  }
  const reponse = await fetch("/api" + chemin, options);
  if (!reponse.ok) {
    if (reponse.status === 401 && chemin !== "/connexion") {
      afficherConnexion();
    }
    throw new ErreurApi(reponse.status, await reponse.text());
  }
  const type = reponse.headers.get("Content-Type") || "";
  return type.includes("json") ? reponse.json() : null;
}

function message(texte, erreur = false) {
  const zone = $("#message");
  zone.textContent = texte;
  zone.classList.toggle("erreur", erreur);
  zone.hidden = !texte;
}

// --- Mise en forme ---

function octets(n) {
  const unites = ["o", "Ko", "Mo", "Go", "To"];
  let i = 0;
  while (n >= 1024 && i < unites.length - 1) {
    n /= 1024;
    i++;
  }
  return `${n.toFixed(i >= 3 ? 1 : 0)} ${unites[i]}`;
}

function duree(secondes) {
  const j = Math.floor(secondes / 86400);
  const h = Math.floor((secondes % 86400) / 3600);
  const m = Math.floor((secondes % 3600) / 60);
  if (j > 0) return `${j} j ${h} h`;
  if (h > 0) return `${h} h ${m} min`;
  return `${m} min`;
}

function ilYa(instant) {
  if (!instant) return "jamais";
  const ecart = Math.max(0, Math.floor(Date.now() / 1000) - instant);
  if (ecart < 60) return "à l'instant";
  return `il y a ${duree(ecart)}`;
}

function date(instant) {
  return new Date(instant * 1000).toLocaleString("fr-FR", {
    day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit",
  });
}

function element(balise, classe, texte) {
  const e = document.createElement(balise);
  if (classe) e.className = classe;
  if (texte !== undefined) e.textContent = texte;
  return e;
}

function jauge(fraction) {
  const barre = element("div", "jauge");
  const rempli = element("span");
  const pourcent = Math.min(100, Math.max(0, fraction * 100));
  rempli.style.width = `${pourcent}%`;
  if (pourcent >= 90) barre.classList.add("critique");
  else if (pourcent >= 75) barre.classList.add("haute");
  barre.append(rempli);
  return barre;
}

// --- Écrans ---

function afficherConnexion() {
  clearInterval(minuterie);
  minuterie = null;
  $("#ecran-tableau").hidden = true;
  $("#identite").hidden = true;
  $("#ecran-connexion").hidden = false;
}

function afficherTableau(compte) {
  $("#compte").textContent = compte;
  $("#identite").hidden = false;
  $("#ecran-connexion").hidden = true;
  $("#ecran-tableau").hidden = false;
  chargerMachines();
  clearInterval(minuterie);
  minuterie = setInterval(() => {
    if (!document.hidden && !$("#onglet-machines").hidden && !terminal) chargerMachines();
  }, PERIODE_MS);
}

// --- Machines ---

function carteMachine(m) {
  const carte = $("#modele-machine").content.firstElementChild.cloneNode(true);
  carte.dataset.id = m.id;
  carte.querySelector(".pastille").classList.add(m.connectee ? "en-ligne" : "hors-ligne");
  carte.querySelector(".pastille").title = m.connectee ? "connectée" : "déconnectée";
  carte.querySelector(".nom").textContent = m.nom || m.id;
  const sousTitre = [m.nom ? m.id : null, m.info?.nom_hote, m.info?.systeme]
    .filter(Boolean)
    .join(" · ");
  carte.querySelector(".sous-titre").textContent = sousTitre;

  const mesures = carte.querySelector(".mesures");
  const ajouter = (titre, valeur, fraction) => {
    const bloc = element("div");
    bloc.append(element("dt", null, titre), element("dd", null, valeur));
    if (fraction !== undefined) bloc.append(jauge(fraction));
    mesures.append(bloc);
  };
  const e = m.etat;
  if (e) {
    ajouter("Processeur", `${e.cpu_pourcent.toFixed(0)} % (${e.nombre_coeurs} cœurs)`,
      e.cpu_pourcent / 100);
    ajouter("Mémoire", `${octets(e.memoire_utilisee)} / ${octets(e.memoire_totale)}`,
      e.memoire_totale ? e.memoire_utilisee / e.memoire_totale : 0);
    ajouter("Charge", e.charge.map((c) => c.toFixed(2)).join("  "));
    ajouter("Allumée depuis", duree(e.allumee_depuis_s));
    const disques = carte.querySelector(".disques");
    for (const d of e.disques) {
      const ligne = element("div", "disque");
      ligne.append(
        element("span", "point", d.point_montage),
        element("span", "taille", `${octets(d.utilise)} / ${octets(d.total)}`),
        jauge(d.total ? d.utilise / d.total : 0),
      );
      disques.append(ligne);
    }
  } else {
    mesures.append(element("p", "vide", "Pas encore d'état reçu."));
  }

  const vu = m.connectee
    ? `Connectée ${ilYa(m.connectee_le)} depuis ${m.adresse} · état ${ilYa(m.etat_recu_le)}`
    : `Déconnectée · vue ${ilYa(m.vue_le)}`;
  carte.querySelector(".vu").textContent = vu;

  const boutonTerminal = carte.querySelector(".ouvrir-terminal");
  boutonTerminal.hidden = !m.info?.terminal;
  boutonTerminal.disabled = !m.connectee;
  boutonTerminal.addEventListener("click", () => ouvrirTerminal(m));

  const actualiser = carte.querySelector(".actualiser");
  actualiser.disabled = !m.connectee;
  actualiser.addEventListener("click", () => actualiserMachine(m.id, actualiser));
  carte.querySelector(".revoquer").addEventListener("click", () => revoquer(m));
  return carte;
}

async function chargerMachines() {
  try {
    const machines = await api("GET", "/machines");
    $("#machines").replaceChildren(...machines.map(carteMachine));
    $("#aucune-machine").hidden = machines.length > 0;
  } catch (erreur) {
    if (erreur.statut !== 401) message(`Machines : ${erreur.message}`, true);
  }
}

async function actualiserMachine(id, bouton) {
  bouton.disabled = true;
  try {
    await api("POST", `/machines/${encodeURIComponent(id)}/actualiser`);
    setTimeout(chargerMachines, 1500);
  } catch (erreur) {
    message(erreur.message, true);
    bouton.disabled = false;
  }
}

async function revoquer(m) {
  const nom = m.nom || m.id;
  if (!confirm(`Révoquer « ${nom} » ? Son jeton ne marchera plus et sa connexion sera coupée.`)) {
    return;
  }
  try {
    await api("DELETE", `/machines/${encodeURIComponent(m.id)}`);
    message(`« ${nom} » révoquée.`);
    chargerMachines();
  } catch (erreur) {
    message(erreur.message, true);
  }
}

async function ajouterMachine(evenement) {
  evenement.preventDefault();
  const formulaire = evenement.target;
  // `formulaire.id` serait l'attribut id du formulaire : passer par `elements`.
  const id = formulaire.elements.namedItem("id").value.trim();
  const nom = formulaire.elements.namedItem("nom").value.trim();
  try {
    const cree = await api("POST", "/machines", { id, nom: nom || null });
    const relais = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/agent`;
    $("#nouveau-id").textContent = cree.id;
    $("#nouvelle-config").textContent =
      `relais = "${relais}"\nid = "${cree.id}"\njeton = "${cree.jeton}"\n`;
    $("#nouveau-jeton").hidden = false;
    formulaire.reset();
    $("#ajout").open = false;
    message("");
    chargerMachines();
  } catch (erreur) {
    message(erreur.message, true);
  }
}

async function copierConfig() {
  try {
    await navigator.clipboard.writeText($("#nouvelle-config").textContent);
    $("#copier").textContent = "Copié ✓";
  } catch {
    // Presse-papiers refusé : sélectionner le texte pour une copie à la main.
    const plage = document.createRange();
    plage.selectNodeContents($("#nouvelle-config"));
    getSelection().removeAllRanges();
    getSelection().addRange(plage);
  }
}

function fermerJeton() {
  $("#nouvelle-config").textContent = "";
  $("#nouveau-jeton").hidden = true;
  $("#copier").textContent = "Copier";
}

// --- Terminal ---
// xterm.js n'est chargé qu'à la première ouverture d'un terminal : l'interface reste légère.

const SEQUENCES = {
  echap: "\x1b", tab: "\t", haut: "\x1b[A", bas: "\x1b[B", droite: "\x1b[C", gauche: "\x1b[D",
  pipe: "|", tilde: "~", tiret: "-", barre: "/",
};
let terminal = null; // { xterm, ajustement, socket, observateur }
let ctrlActif = false;
let chargementXterm = null;

function chargerScript(src) {
  return new Promise((resoudre, rejeter) => {
    const script = document.createElement("script");
    script.src = src;
    script.onload = resoudre;
    script.onerror = () => rejeter(new Error(`chargement de ${src} impossible`));
    document.head.append(script);
  });
}

function chargerXterm() {
  if (!chargementXterm) {
    const style = document.createElement("link");
    style.rel = "stylesheet";
    style.href = "/vendor/xterm.css";
    document.head.append(style);
    chargementXterm = Promise.all([
      chargerScript("/vendor/xterm.js"),
      chargerScript("/vendor/addon-fit.js"),
    ]).catch((erreur) => {
      chargementXterm = null;
      throw erreur;
    });
  }
  return chargementXterm;
}

function basculerCtrl(actif) {
  ctrlActif = actif;
  $("#touche-ctrl").classList.toggle("actif", actif);
}

/// Avec Ctrl enfoncé (bouton de l'écran), une lettre devient le caractère de contrôle.
function appliquerCtrl(donnees) {
  if (!ctrlActif || donnees.length !== 1) return donnees;
  basculerCtrl(false);
  const code = donnees.toUpperCase().charCodeAt(0);
  return code >= 64 && code <= 95 ? String.fromCharCode(code - 64) : donnees;
}

function envoyerAuTerminal(texte) {
  if (terminal?.socket.readyState === WebSocket.OPEN) {
    terminal.socket.send(new TextEncoder().encode(texte));
  }
}

async function ouvrirTerminal(m) {
  if (terminal) return;
  try {
    await chargerXterm();
  } catch (erreur) {
    message(erreur.message, true);
    return;
  }
  $("#titre-terminal").textContent = m.nom || m.id;
  $("#etat-terminal").textContent = "connexion…";
  $("#ecran-terminal").hidden = false;
  document.body.classList.add("terminal-ouvert");
  basculerCtrl(false);

  const xterm = new Terminal({
    cursorBlink: true,
    fontFamily: "ui-monospace, 'DejaVu Sans Mono', Menlo, monospace",
    fontSize: window.innerWidth < 600 ? 13 : 15,
    scrollback: 5000,
    theme: { background: "#0b1020" },
  });
  const ajustement = new FitAddon.FitAddon();
  xterm.loadAddon(ajustement);
  xterm.open($("#terminal"));
  ajustement.fit();

  const schema = location.protocol === "https:" ? "wss" : "ws";
  const url = `${schema}://${location.host}/api/machines/${encodeURIComponent(m.id)}/terminal` +
    `?colonnes=${xterm.cols}&lignes=${xterm.rows}`;
  const socket = new WebSocket(url);
  socket.binaryType = "arraybuffer";
  const observateur = new ResizeObserver(() => ajustement.fit());
  observateur.observe($("#terminal"));
  terminal = { xterm, ajustement, socket, observateur };

  socket.onopen = () => {
    $("#etat-terminal").textContent = "";
    xterm.focus();
  };
  socket.onmessage = (evenement) => {
    if (evenement.data instanceof ArrayBuffer) xterm.write(new Uint8Array(evenement.data));
  };
  socket.onclose = (evenement) => {
    const raison = evenement.reason || "connexion perdue";
    $("#etat-terminal").textContent = `fermé : ${raison}`;
    xterm.write(`\r\n\x1b[90m[terminal fermé : ${raison}]\x1b[0m\r\n`);
  };
  xterm.onData((donnees) => envoyerAuTerminal(appliquerCtrl(donnees)));
  xterm.onResize(({ cols, rows }) => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ colonnes: cols, lignes: rows }));
    }
  });
}

function fermerTerminal() {
  if (!terminal) return;
  terminal.observateur.disconnect();
  terminal.socket.onclose = null;
  terminal.socket.close();
  terminal.xterm.dispose();
  terminal = null;
  $("#ecran-terminal").hidden = true;
  document.body.classList.remove("terminal-ouvert");
  chargerMachines();
}

function toucheEcran(nom) {
  if (!terminal) return;
  if (nom === "ctrl") basculerCtrl(!ctrlActif);
  else envoyerAuTerminal(appliquerCtrl(SEQUENCES[nom]));
  terminal.xterm.focus();
}

// --- Journal ---

const ACTIONS = {
  connexion: "Connexion",
  connexion_refusee: "Connexion refusée",
  connexion_bloquee: "Connexion bloquée (trop d'essais)",
  deconnexion: "Déconnexion",
  machine_ajoutee: "Machine ajoutée",
  machine_revoquee: "Machine révoquée",
  etat_demande: "État demandé",
  agent_connecte: "Agent connecté",
  agent_deconnecte: "Agent déconnecté",
  agent_refuse: "Agent refusé",
  terminal_ouvert: "Terminal ouvert",
  terminal_ferme: "Terminal fermé",
  compte_cree: "Compte créé",
  compte_supprime: "Compte supprimé",
};

async function chargerJournal() {
  try {
    const lignes = await api("GET", "/audit?limite=200");
    $("#journal").replaceChildren(...lignes.map((l) => {
      const li = element("li", l.action.includes("refus") || l.action.includes("bloq")
        ? "alerte" : "");
      const titre = ACTIONS[l.action] || l.action;
      li.append(
        element("time", null, date(l.instant)),
        element("strong", null, l.detail ? `${titre} : ${l.detail}` : titre),
        element("span", "qui", `${l.compte || "—"} · ${l.adresse}`),
      );
      return li;
    }));
  } catch (erreur) {
    if (erreur.statut !== 401) message(`Journal : ${erreur.message}`, true);
  }
}

function changerOnglet(nom) {
  for (const bouton of document.querySelectorAll("[data-onglet]")) {
    bouton.classList.toggle("actif", bouton.dataset.onglet === nom);
  }
  $("#onglet-machines").hidden = nom !== "machines";
  $("#onglet-journal").hidden = nom !== "journal";
  if (nom === "journal") chargerJournal();
  else chargerMachines();
}

// --- Connexion ---

async function seConnecter(evenement) {
  evenement.preventDefault();
  const f = evenement.target;
  const bouton = f.querySelector("button");
  bouton.disabled = true;
  try {
    const reponse = await api("POST", "/connexion", {
      nom: f.nom.value.trim(),
      mot_de_passe: f.mot_de_passe.value,
      code: f.code.value.trim(),
    });
    f.reset();
    message("");
    afficherTableau(reponse.compte);
  } catch (erreur) {
    f.code.value = "";
    message(erreur.statut === 401 ? "Compte, mot de passe ou code refusé." : erreur.message, true);
  } finally {
    bouton.disabled = false;
  }
}

async function seDeconnecter() {
  try {
    await api("POST", "/deconnexion");
  } catch {
    // Session déjà expirée : rien à faire.
  }
  $("#machines").replaceChildren();
  $("#journal").replaceChildren();
  fermerJeton();
  fermerTerminal();
  afficherConnexion();
}

async function demarrer() {
  $("#formulaire-connexion").addEventListener("submit", seConnecter);
  $("#formulaire-ajout").addEventListener("submit", ajouterMachine);
  $("#deconnexion").addEventListener("click", seDeconnecter);
  $("#copier").addEventListener("click", copierConfig);
  $("#fermer-jeton").addEventListener("click", fermerJeton);
  $("#fermer-terminal").addEventListener("click", fermerTerminal);
  for (const bouton of document.querySelectorAll("[data-touche]")) {
    // Garder le focus (et le clavier du téléphone) sur le terminal.
    bouton.addEventListener("mousedown", (e) => e.preventDefault());
    bouton.addEventListener("click", () => toucheEcran(bouton.dataset.touche));
  }
  window.visualViewport?.addEventListener("resize", () => {
    // Le clavier du téléphone réduit la zone visible : le terminal s'y adapte.
    $("#ecran-terminal").style.height = `${window.visualViewport.height}px`;
    terminal?.ajustement.fit();
  });
  for (const bouton of document.querySelectorAll("[data-onglet]")) {
    bouton.addEventListener("click", () => changerOnglet(bouton.dataset.onglet));
  }
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && minuterie && !$("#onglet-machines").hidden) chargerMachines();
  });
  try {
    const moi = await api("GET", "/moi");
    afficherTableau(moi.compte);
  } catch {
    afficherConnexion();
  }
}

demarrer();
