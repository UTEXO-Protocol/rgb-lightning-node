use super::*;

pub(crate) async fn backup(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<BackupRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _guard = state.check_locked().await?;

        let _mnemonic =
            check_password_validity(&payload.password, &state.static_state.storage_dir_path)?;

        do_backup(
            &state.static_state.storage_dir_path,
            Path::new(&payload.backup_path),
            &payload.password,
        )?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn change_password(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<ChangePasswordRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _guard = state.check_locked().await?;

        check_password_strength(payload.new_password.clone())?;

        let mnemonic =
            check_password_validity(&payload.old_password, &state.static_state.storage_dir_path)?;

        encrypt_and_save_mnemonic(
            payload.new_password,
            mnemonic.to_string(),
            &get_mnemonic_path(&state.static_state.storage_dir_path),
        )?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn init(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<InitRequest>, APIError>,
) -> Result<Json<InitResponse>, APIError> {
    no_cancel(async move {
        let _unlocked_state = state.check_locked().await?;

        check_password_strength(payload.password.clone())?;

        let mnemonic_path = get_mnemonic_path(&state.static_state.storage_dir_path);
        check_already_initialized(&mnemonic_path)?;

        let keys = generate_keys(state.static_state.network);

        let mnemonic = keys.mnemonic;

        encrypt_and_save_mnemonic(payload.password, mnemonic.clone(), &mnemonic_path)?;

        Ok(Json(InitResponse { mnemonic }))
    })
    .await
}

pub(crate) async fn lock(
    State(state): State<Arc<AppState>>,
) -> Result<Json<EmptyResponse>, APIError> {
    tracing::info!("Lock started");
    no_cancel(async move {
        match state.check_unlocked().await {
            Ok(unlocked_state) => {
                state.update_changing_state(true);
                drop(unlocked_state);
            }
            Err(e) => {
                state.update_changing_state(false);
                return Err(e);
            }
        }

        tracing::debug!("Stopping LDK...");
        stop_ldk(state.clone()).await;
        tracing::debug!("LDK stopped");

        state.update_unlocked_app_state(None).await;

        state.update_ldk_background_services(None);

        state.update_changing_state(false);

        tracing::info!("Lock completed");
        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn restore(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RestoreRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _unlocked_state = state.check_locked().await?;

        let mnemonic_path = get_mnemonic_path(&state.static_state.storage_dir_path);
        check_already_initialized(&mnemonic_path)?;

        restore_backup(
            Path::new(&payload.backup_path),
            &payload.password,
            &state.static_state.storage_dir_path,
        )?;

        let _mnemonic =
            check_password_validity(&payload.password, &state.static_state.storage_dir_path)?;

        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn revoke_token(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<RevokeTokenRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    let Some(root_pubkey) = state.root_public_key else {
        return Err(APIError::AuthenticationDisabled);
    };

    let token_to_revoke = Biscuit::from_base64(&payload.token, root_pubkey)
        .map_err(|_| APIError::InvalidBiscuitToken)?;
    state.revoke_token(&token_to_revoke)?;

    Ok(Json(EmptyResponse {}))
}

pub(crate) async fn shutdown(
    State(state): State<Arc<AppState>>,
) -> Result<Json<EmptyResponse>, APIError> {
    no_cancel(async move {
        let _unlocked_app_state = state.get_unlocked_app_state();
        state.check_changing_state()?;

        state.cancel_token.cancel();
        Ok(Json(EmptyResponse {}))
    })
    .await
}

pub(crate) async fn unlock(
    State(state): State<Arc<AppState>>,
    WithRejection(Json(payload), _): WithRejection<Json<UnlockRequest>, APIError>,
) -> Result<Json<EmptyResponse>, APIError> {
    tracing::info!("Unlock started");
    no_cancel(async move {
        match state.check_locked().await {
            Ok(unlocked_state) => {
                state.update_changing_state(true);
                drop(unlocked_state);
            }
            Err(e) => {
                return Err(match e {
                    APIError::UnlockedNode => APIError::AlreadyUnlocked,
                    _ => e,
                });
            }
        }

        let mnemonic = match check_password_validity(
            &payload.password,
            &state.static_state.storage_dir_path,
        ) {
            Ok(mnemonic) => mnemonic,
            Err(e) => {
                state.update_changing_state(false);
                return Err(e);
            }
        };

        tracing::debug!("Starting LDK...");
        let (new_ldk_background_services, new_unlocked_app_state) =
            match start_ldk(state.clone(), mnemonic, payload).await {
                Ok((nlbs, nuap)) => (nlbs, nuap),
                Err(e) => {
                    state.update_changing_state(false);
                    return Err(e);
                }
            };
        tracing::debug!("LDK started");

        state
            .update_unlocked_app_state(Some(new_unlocked_app_state))
            .await;

        state.update_ldk_background_services(Some(new_ldk_background_services));

        state.update_changing_state(false);

        tracing::info!("Unlock completed");
        Ok(Json(EmptyResponse {}))
    })
    .await
}
