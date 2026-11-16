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
    if (!document.hidden && !$("#onglet-machines").hidden) chargerMachines();
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
  afficherConnexion();
}

async function demarrer() {
  $("#formulaire-connexion").addEventListener("submit", seConnecter);
  $("#formulaire-ajout").addEventListener("submit", ajouterMachine);
  $("#deconnexion").addEventListener("click", seDeconnecter);
  $("#copier").addEventListener("click", copierConfig);
  $("#fermer-jeton").addEventListener("click", fermerJeton);
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
