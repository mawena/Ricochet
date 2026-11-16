//! API de l'opérateur.
//!
//! Accès par la session de l'interface (cookie) ou, pour les scripts, par le jeton de la
//! configuration (`Authorization: Bearer <jeton>`). Avec le cookie, toute requête qui modifie
//! quelque chose doit porter l'en-tête `X-Ricochet` : un autre site ne peut pas l'ajouter sans
//! l'accord du relais, ce qui bloque les requêtes forgées (CSRF).

use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use ricochet_protocole::RelaisVersAgent;
use serde::Deserialize;
use serde_json::json;

use crate::{
    Adresse, Relais, comptes, config, fichiers, jeton,
    registre::{Commande, ErreurEnvoi, VueMachine, maintenant},
    terminal, totp,
};

/// Nom du compte inscrit au journal pour les appels faits avec le jeton de la configuration.
const COMPTE_JETON: &str = "jeton-api";

/// L'opérateur authentifié d'une requête.
#[derive(Clone)]
pub(crate) struct Operateur {
    pub compte: String,
    /// Empreinte de la session (absente avec le jeton de la configuration).
    pub session: Option<String>,
}

/// Erreur interne : journalisée, renvoyée sans détail.
pub(crate) struct Erreur(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for Erreur {
    fn from(erreur: E) -> Self {
        Self(erreur.into())
    }
}

impl IntoResponse for Erreur {
    fn into_response(self) -> Response {
        tracing::error!("erreur interne : {:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, "erreur interne").into_response()
    }
}

type Resultat = Result<Response, Erreur>;

pub fn routeur(relais: Arc<Relais>) -> Router<Arc<Relais>> {
    let protegees = Router::new()
        .route("/deconnexion", post(deconnexion))
        .route("/moi", get(moi))
        .route("/machines", get(machines).post(ajouter_machine))
        .route("/machines/{id}", delete(revoquer_machine))
        .route("/machines/{id}/actualiser", post(actualiser))
        .route("/machines/{id}/terminal", get(terminal::connexion))
        .route(
            "/machines/{id}/fichiers",
            get(fichiers::lister).put(fichiers::envoyer),
        )
        .route("/machines/{id}/fichiers/partiel", get(fichiers::partiel))
        .route(
            "/machines/{id}/fichiers/telecharger",
            get(fichiers::telecharger),
        )
        .route("/audit", get(audit))
        .layer(middleware::from_fn_with_state(relais, authentifier));
    Router::new()
        .route("/connexion", post(connexion))
        .merge(protegees)
        .layer(middleware::from_fn(contre_csrf))
}

/// Nom du cookie de session : le préfixe `__Host-` l'attache au seul domaine du relais, en HTTPS.
fn nom_cookie(relais: &Relais) -> &'static str {
    if relais.config.cookie_securise {
        "__Host-ricochet"
    } else {
        "ricochet"
    }
}

fn cookie_recu(relais: &Relais, entetes: &HeaderMap) -> Option<String> {
    let nom = nom_cookie(relais);
    entetes
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|paire| paire.trim().split_once('='))
        .find(|(n, _)| *n == nom)
        .map(|(_, valeur)| valeur.to_owned())
}

fn bearer(entetes: &HeaderMap) -> Option<&str> {
    entetes
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Refuse les requêtes qui modifient quelque chose sans l'en-tête `X-Ricochet`, sauf avec le
/// jeton de la configuration (qu'un navigateur n'envoie jamais de lui-même).
async fn contre_csrf(requete: Request, suite: Next) -> Response {
    let lecture = matches!(*requete.method(), Method::GET | Method::HEAD);
    if !lecture
        && bearer(requete.headers()).is_none()
        && !requete.headers().contains_key("x-ricochet")
    {
        return (StatusCode::FORBIDDEN, "en-tête X-Ricochet manquant").into_response();
    }
    suite.run(requete).await
}

async fn authentifier(
    State(relais): State<Arc<Relais>>,
    mut requete: Request,
    suite: Next,
) -> Response {
    let operateur = if let Some(recu) = bearer(requete.headers()) {
        match &relais.config.jeton_operateur_sha256 {
            Some(empreinte) if jeton::verifier(recu, empreinte) => Some(Operateur {
                compte: COMPTE_JETON.into(),
                session: None,
            }),
            _ => None,
        }
    } else if let Some(valeur) = cookie_recu(&relais, requete.headers()) {
        let empreinte = jeton::empreinte(&valeur);
        match relais.base.session(&empreinte, maintenant()) {
            Ok(compte) => compte.map(|compte| Operateur {
                compte,
                session: Some(empreinte),
            }),
            Err(erreur) => return Erreur(erreur).into_response(),
        }
    } else {
        None
    };
    match operateur {
        Some(operateur) => {
            requete.extensions_mut().insert(operateur);
            suite.run(requete).await
        }
        None => (StatusCode::UNAUTHORIZED, "connexion requise").into_response(),
    }
}

#[derive(Deserialize)]
struct Identifiants {
    nom: String,
    mot_de_passe: String,
    code: String,
}

/// Connexion : mot de passe et code TOTP ensemble. La réponse ne dit jamais lequel est faux.
async fn connexion(
    State(relais): State<Arc<Relais>>,
    Adresse(adresse): Adresse,
    Json(identifiants): Json<Identifiants>,
) -> Resultat {
    let instant = maintenant();
    let cle_adresse = format!("adresse:{adresse}");
    let cle_compte = format!("compte:{}", identifiants.nom);
    if relais.essais_adresse.bloque(&cle_adresse, instant)
        || relais.essais_compte.bloque(&cle_compte, instant)
    {
        relais.noter(None, &adresse, "connexion_bloquee", &identifiants.nom);
        let minutes = comptes::FENETRE_S / 60;
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            format!("trop d'essais, réessayez dans {minutes} minutes"),
        )
            .into_response());
    }

    let compte = relais.base.compte(&identifiants.nom)?;
    let empreinte = compte.as_ref().map(|c| c.mot_de_passe.clone());
    let mot_de_passe = identifiants.mot_de_passe;
    let mot_de_passe_bon =
        tokio::task::spawn_blocking(move || comptes::verifier(&mot_de_passe, empreinte.as_deref()))
            .await?;
    let accepte = match &compte {
        Some(compte) if mot_de_passe_bon => {
            match totp::verifier(
                &compte.totp_secret,
                &identifiants.code,
                instant,
                compte.totp_dernier_pas,
            ) {
                Some(pas) => relais.base.noter_pas_totp(&compte.nom, pas)?,
                None => false,
            }
        }
        _ => false,
    };
    if !accepte {
        relais.essais_adresse.echec(&cle_adresse, instant);
        relais.essais_compte.echec(&cle_compte, instant);
        relais.noter(None, &adresse, "connexion_refusee", &identifiants.nom);
        return Ok((StatusCode::UNAUTHORIZED, "identifiants refusés").into_response());
    }

    relais.essais_compte.oublier(&cle_compte);
    let valeur = jeton::nouveau()?;
    let duree_s = relais.config.duree_session_h * 3600;
    relais.base.creer_session(
        &jeton::empreinte(&valeur),
        &identifiants.nom,
        instant,
        instant + duree_s,
        &adresse,
    )?;
    relais.noter(Some(&identifiants.nom), &adresse, "connexion", "");
    let securise = if relais.config.cookie_securise {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{}={valeur}; Path=/; HttpOnly; SameSite=Strict; Max-Age={duree_s}{securise}",
        nom_cookie(&relais)
    );
    Ok((
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie)?)],
        Json(json!({ "compte": identifiants.nom })),
    )
        .into_response())
}

async fn deconnexion(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
) -> Resultat {
    if let Some(session) = &operateur.session {
        relais.base.supprimer_session(session)?;
    }
    relais.noter(Some(&operateur.compte), &adresse, "deconnexion", "");
    let cookie = format!("{}=; Path=/; Max-Age=0", nom_cookie(&relais));
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, HeaderValue::from_str(&cookie)?)],
    )
        .into_response())
}

async fn moi(Extension(operateur): Extension<Operateur>) -> Json<serde_json::Value> {
    Json(json!({ "compte": operateur.compte }))
}

async fn machines(State(relais): State<Arc<Relais>>) -> Result<Json<Vec<VueMachine>>, Erreur> {
    Ok(Json(relais.registre.liste(&relais.base.machines()?)))
}

#[derive(Deserialize)]
struct NouvelleMachine {
    id: String,
    #[serde(default)]
    nom: Option<String>,
}

/// Déclare une machine et renvoie son jeton : il n'est montré qu'une fois, seule son empreinte
/// est gardée.
async fn ajouter_machine(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Json(nouvelle): Json<NouvelleMachine>,
) -> Resultat {
    if !config::identifiant_valide(&nouvelle.id) {
        return Ok((
            StatusCode::BAD_REQUEST,
            "identifiant invalide : lettres, chiffres, - et _ (64 au plus)",
        )
            .into_response());
    }
    let nom = nouvelle
        .nom
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if nom.is_some_and(|n| n.chars().count() > 100) {
        return Ok((StatusCode::BAD_REQUEST, "nom trop long (100 au plus)").into_response());
    }
    let valeur = jeton::nouveau()?;
    if !relais
        .base
        .ajouter_machine(&nouvelle.id, nom, &jeton::empreinte(&valeur), maintenant())?
    {
        return Ok((StatusCode::CONFLICT, "cet identifiant existe déjà").into_response());
    }
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "machine_ajoutee",
        &nouvelle.id,
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": nouvelle.id, "jeton": valeur })),
    )
        .into_response())
}

/// Révoque une machine : son jeton ne vaut plus rien et sa connexion est fermée.
async fn revoquer_machine(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
) -> Resultat {
    if !relais.base.supprimer_machine(&id)? {
        return Ok((StatusCode::NOT_FOUND, "machine inconnue").into_response());
    }
    relais.registre.retirer(&id);
    relais.noter(Some(&operateur.compte), &adresse, "machine_revoquee", &id);
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Demande à une machine connectée d'envoyer son état sans attendre.
async fn actualiser(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
) -> Resultat {
    if relais.base.empreinte_machine(&id)?.is_none() {
        return Ok((StatusCode::NOT_FOUND, "machine inconnue").into_response());
    }
    relais.noter(Some(&operateur.compte), &adresse, "etat_demande", &id);
    Ok(
        match relais
            .registre
            .envoyer(&id, Commande::Envoyer(RelaisVersAgent::DemandeEtat))
        {
            Ok(()) => StatusCode::ACCEPTED.into_response(),
            Err(ErreurEnvoi::Deconnectee) => {
                (StatusCode::CONFLICT, "machine déconnectée").into_response()
            }
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "machine occupée").into_response(),
        },
    )
}

#[derive(Deserialize)]
struct ParametresAudit {
    limite: Option<u32>,
}

async fn audit(
    State(relais): State<Arc<Relais>>,
    Query(parametres): Query<ParametresAudit>,
) -> Resultat {
    let limite = parametres.limite.unwrap_or(200).clamp(1, 1000);
    Ok(Json(relais.base.journal(limite)?).into_response())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;

    use crate::{Base, Config, Relais, application, comptes, jeton, totp};

    const SECRET: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

    fn relais() -> Arc<Relais> {
        let config = Config::depuis_texte(&format!(
            "jeton_operateur_sha256 = \"{}\"\ncookie_securise = false\n",
            jeton::empreinte("operateur"),
        ))
        .unwrap();
        let base = Base::en_memoire();
        let empreinte = comptes::hacher("mot de passe solide").unwrap();
        base.creer_compte("charles", &empreinte, SECRET, 0).unwrap();
        base.ajouter_machine("pc", Some("Mon PC"), &jeton::empreinte("agent"), 0)
            .unwrap();
        Arc::new(Relais::new(config, base))
    }

    /// Code TOTP valable maintenant pour `SECRET`.
    fn code_actuel() -> String {
        totp::code_pour(SECRET, crate::registre::maintenant())
    }

    struct Reponse {
        statut: u16,
        cookie: Option<String>,
        corps: String,
    }

    async fn appeler(
        relais: &Arc<Relais>,
        methode: &str,
        chemin: &str,
        entetes: &[(&str, &str)],
        corps: Option<serde_json::Value>,
    ) -> Reponse {
        let mut requete = axum::http::Request::builder().method(methode).uri(chemin);
        for (nom, valeur) in entetes {
            requete = requete.header(*nom, *valeur);
        }
        let corps = match corps {
            Some(valeur) => {
                requete = requete.header("content-type", "application/json");
                Body::from(valeur.to_string())
            }
            None => Body::empty(),
        };
        let reponse = application(relais.clone())
            .oneshot(requete.body(corps).unwrap())
            .await
            .unwrap();
        let statut = reponse.status().as_u16();
        let cookie = reponse
            .headers()
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_owned());
        let corps = to_bytes(reponse.into_body(), 1 << 20).await.unwrap();
        Reponse {
            statut,
            cookie,
            corps: String::from_utf8(corps.to_vec()).unwrap(),
        }
    }

    const BEARER: (&str, &str) = ("authorization", "Bearer operateur");

    async fn se_connecter(relais: &Arc<Relais>, mot_de_passe: &str, code: &str) -> Reponse {
        appeler(
            relais,
            "POST",
            "/api/connexion",
            &[("x-ricochet", "1")],
            Some(serde_json::json!({"nom": "charles", "mot_de_passe": mot_de_passe, "code": code})),
        )
        .await
    }

    #[tokio::test]
    async fn sante_et_interface_sans_connexion() {
        let relais = relais();
        assert_eq!(
            appeler(&relais, "GET", "/sante", &[], None).await.corps,
            "ok"
        );
        let accueil = appeler(&relais, "GET", "/", &[], None).await;
        assert_eq!(accueil.statut, 200);
        assert!(accueil.corps.contains("Ricochet"));
    }

    #[tokio::test]
    async fn authentification_obligatoire() {
        let relais = relais();
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &[], None)
                .await
                .statut,
            401
        );
        let faux = [("authorization", "Bearer faux")];
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &faux, None)
                .await
                .statut,
            401
        );
        // Le jeton d'un agent n'ouvre pas l'API.
        let agent = [("authorization", "Bearer agent")];
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &agent, None)
                .await
                .statut,
            401
        );
        let cookie = [("cookie", "ricochet=inconnu")];
        assert_eq!(
            appeler(&relais, "GET", "/api/machines", &cookie, None)
                .await
                .statut,
            401
        );
    }

    #[tokio::test]
    async fn connexion_complete() {
        let relais = relais();
        assert_eq!(
            se_connecter(&relais, "mot de passe solide", "000000")
                .await
                .statut,
            401
        );
        let code = code_actuel();
        assert_eq!(se_connecter(&relais, "mauvais", &code).await.statut, 401);

        let reponse = se_connecter(&relais, "mot de passe solide", &code).await;
        assert_eq!(reponse.statut, 200, "{}", reponse.corps);
        let cookie = reponse.cookie.unwrap();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
        let valeur = cookie.split(';').next().unwrap().to_owned();

        // Le même code ne sert pas deux fois.
        assert_eq!(
            se_connecter(&relais, "mot de passe solide", &code)
                .await
                .statut,
            401
        );

        let session = [("cookie", valeur.as_str())];
        let moi = appeler(&relais, "GET", "/api/moi", &session, None).await;
        assert_eq!(moi.statut, 200);
        assert!(moi.corps.contains("charles"));

        // Sans l'en-tête X-Ricochet, une requête qui modifie est refusée (CSRF).
        let sortie = appeler(&relais, "POST", "/api/deconnexion", &session, None).await;
        assert_eq!(sortie.statut, 403);
        let session_csrf = [("cookie", valeur.as_str()), ("x-ricochet", "1")];
        let sortie = appeler(&relais, "POST", "/api/deconnexion", &session_csrf, None).await;
        assert_eq!(sortie.statut, 204);
        assert_eq!(
            appeler(&relais, "GET", "/api/moi", &session, None)
                .await
                .statut,
            401
        );

        let actions: Vec<String> = relais
            .base
            .journal(10)
            .unwrap()
            .into_iter()
            .map(|l| l.action)
            .collect();
        assert_eq!(
            actions,
            [
                "deconnexion",
                "connexion_refusee",
                "connexion",
                "connexion_refusee",
                "connexion_refusee"
            ]
        );
    }

    #[tokio::test]
    async fn trop_d_essais() {
        let relais = relais();
        for _ in 0..comptes::ECHECS_MAX_ADRESSE {
            assert_eq!(se_connecter(&relais, "faux", "000000").await.statut, 401);
        }
        // Même le bon mot de passe est refusé tant que l'adresse est bloquée.
        let code = code_actuel();
        let reponse = se_connecter(&relais, "mot de passe solide", &code).await;
        assert_eq!(reponse.statut, 429);
    }

    #[tokio::test]
    async fn liste_des_machines() {
        let relais = relais();
        let reponse = appeler(&relais, "GET", "/api/machines", &[BEARER], None).await;
        assert_eq!(reponse.statut, 200);
        let liste: serde_json::Value = serde_json::from_str(&reponse.corps).unwrap();
        assert_eq!(liste[0]["id"], "pc");
        assert_eq!(liste[0]["nom"], "Mon PC");
        assert_eq!(liste[0]["connectee"], false);
    }

    #[tokio::test]
    async fn ajout_et_revocation() {
        let relais = relais();
        let ajout = |id: &str| serde_json::json!({ "id": id, "nom": "Serveur" });
        let reponse = appeler(
            &relais,
            "POST",
            "/api/machines",
            &[BEARER],
            Some(ajout("srv")),
        )
        .await;
        assert_eq!(reponse.statut, 201);
        let cree: serde_json::Value = serde_json::from_str(&reponse.corps).unwrap();
        let jeton_srv = cree["jeton"].as_str().unwrap();
        assert!(jeton::verifier(
            jeton_srv,
            &relais.base.empreinte_machine("srv").unwrap().unwrap()
        ));

        let doublon = appeler(
            &relais,
            "POST",
            "/api/machines",
            &[BEARER],
            Some(ajout("srv")),
        )
        .await;
        assert_eq!(doublon.statut, 409);
        let invalide = appeler(
            &relais,
            "POST",
            "/api/machines",
            &[BEARER],
            Some(ajout("a b")),
        )
        .await;
        assert_eq!(invalide.statut, 400);

        assert_eq!(
            appeler(&relais, "DELETE", "/api/machines/srv", &[BEARER], None)
                .await
                .statut,
            204
        );
        assert_eq!(
            appeler(&relais, "DELETE", "/api/machines/srv", &[BEARER], None)
                .await
                .statut,
            404
        );
        assert!(relais.base.empreinte_machine("srv").unwrap().is_none());

        let journal = appeler(&relais, "GET", "/api/audit?limite=5", &[BEARER], None).await;
        let lignes: serde_json::Value = serde_json::from_str(&journal.corps).unwrap();
        assert_eq!(lignes[0]["action"], "machine_revoquee");
        assert_eq!(lignes[0]["compte"], "jeton-api");
        assert_eq!(lignes[1]["action"], "machine_ajoutee");
    }

    #[tokio::test]
    async fn actualiser_machine() {
        let relais = relais();
        let inconnue = "/api/machines/inconnue/actualiser";
        assert_eq!(
            appeler(&relais, "POST", inconnue, &[BEARER], None)
                .await
                .statut,
            404
        );
        let pc = "/api/machines/pc/actualiser";
        assert_eq!(
            appeler(&relais, "POST", pc, &[BEARER], None).await.statut,
            409
        );
    }

    #[tokio::test]
    async fn entetes_de_securite() {
        let relais = relais();
        let reponse = application(relais)
            .oneshot(axum::http::Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let csp = reponse.headers()["content-security-policy"]
            .to_str()
            .unwrap();
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(
            csp.contains("script-src 'self';"),
            "aucun script en ligne : {csp}"
        );
        assert_eq!(reponse.headers()["x-frame-options"], "DENY");
    }
}
