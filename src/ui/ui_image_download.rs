use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    steam::{get_shortcuts_paths, SteamUsersInfo},
    steamgriddb::{get_query_type, CachedSearch, ImageType, ToDownload},
};
use dashmap::DashMap;
use egui::{ImageButton, ScrollArea, TextureHandle};
use futures::executor::block_on;
use steam_shortcuts_util::shortcut::ShortcutOwned;
use tokio::sync::watch::{self, Receiver};

use super::{ui_images::load_image_from_path, FetcStatus, MyEguiApp};

pub struct ImageSelectState {
    pub selected_shortcut: Option<ShortcutOwned>,
    pub grid_id: Option<usize>,

    pub hero_image: Option<egui::TextureHandle>,
    pub grid_image: Option<egui::TextureHandle>,
    pub logo_image: Option<egui::TextureHandle>,
    pub icon_image: Option<egui::TextureHandle>,
    pub wide_image: Option<egui::TextureHandle>,

    pub steam_user: Option<SteamUsersInfo>,
    pub steam_users: Option<Vec<SteamUsersInfo>>,
    pub user_shortcuts: Option<Vec<ShortcutOwned>>,

    pub image_type_selected: Option<ImageType>,
    pub image_options: Receiver<FetcStatus<Vec<PossibleImage>>>,

    pub image_handles: std::sync::Arc<DashMap<String, TextureState>>,

    pub possible_names: Option<Vec<steamgriddb_api::search::SearchResult>>,
}

pub enum TextureState {
    Downloading,
    Downloaded,
    Loaded(egui::TextureHandle),
}

impl ImageSelectState {
    pub fn has_multiple_users(&self) -> bool {
        match &self.steam_users {
            Some(users) => users.len() > 1,
            None => false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PossibleImage {
    thumbnail_path: PathBuf,
    thumbnail_url: String,
    full_url: String,
    id: u32,
}

impl Default for ImageSelectState {
    fn default() -> Self {
        Self {
            selected_shortcut: Default::default(),
            grid_id: Default::default(),
            hero_image: Default::default(),
            grid_image: Default::default(),
            logo_image: Default::default(),
            icon_image: Default::default(),
            wide_image: Default::default(),
            steam_user: Default::default(),
            steam_users: Default::default(),
            user_shortcuts: Default::default(),
            image_type_selected: Default::default(),
            possible_names: None,
            image_options: watch::channel(FetcStatus::NeedsFetched).1,
            image_handles: Arc::new(DashMap::new()),
        }
    }
}

#[derive(Debug)]
enum UserAction {
    CorrectGridId,
    UserSelected(SteamUsersInfo),
    ShortcutSelected(ShortcutOwned),
    ImageTypeSelected(ImageType),
    ImageSelected(PossibleImage),
    GridIdChanged(usize),
    BackButton,
    NoAction,
}

impl MyEguiApp {
    fn render_ui_image_action(&self, ui: &mut egui::Ui) -> UserAction {
        let state = &self.image_selected_state;
        ui.heading("Images");
        if (state.selected_shortcut.is_some() || (state.has_multiple_users() && state.steam_user.is_some()))
            && ui.button("Back").clicked()
        {
            return UserAction::BackButton;
        }
        if state.steam_user.is_none() {
            return render_user_select(state, ui);
        }
        if let Some(shortcut) = state.selected_shortcut.as_ref() {
            ui.heading(&shortcut.app_name);

            if let Some(possible_names) = state.possible_names.as_ref() {
                if let Some(value) = render_possible_names(possible_names, ui) {
                    return value;
                }
            } else {
                if let Some(image_type) = state.image_type_selected.as_ref() {
                    if let Some(action) = self.render_possible_images(ui, image_type, state) {
                        return action;
                    }
                } else if let Some(action) = render_shortcut_images(ui, state) {
                    return action;
                }
            }
        } else if let Some(action) = self.render_shortcut_select(ui) {
            return action;
        }
        UserAction::NoAction
    }

    fn render_shortcut_select(&self, ui: &mut egui::Ui) -> Option<UserAction> {
        let shortcuts = &self.image_selected_state.user_shortcuts;
        match shortcuts {
            Some(shortcuts) => {
                for shortcut in shortcuts {
                    if ui.button(&shortcut.app_name).clicked() {
                        return Some(UserAction::ShortcutSelected(shortcut.clone()));
                    }
                }
            }
            None => {
                ui.label("Could not find any shortcuts");
            }
        }
        None
    }

    fn render_possible_images(
        &self,
        ui: &mut egui::Ui,
        image_type: &ImageType,
        state: &ImageSelectState,
    ) -> Option<UserAction> {
        ui.heading(image_type.name());

        match &*state.image_options.borrow() {
            FetcStatus::Fetched(images) => {
                for image in images {
                    let image_key = image.thumbnail_path.as_path().to_string_lossy().to_string();

                    match state.image_handles.get_mut(&image_key) {
                        Some(mut state) => {
                            match state.value() {
                                TextureState::Downloading => {
                                    ui.ctx().request_repaint();
                                    //nothing to do,just wait
                                    ui.label(format!("Downloading id {}", image.id));
                                }
                                TextureState::Downloaded => {
                                    //Need to load
                                    let image_data =
                                        load_image_from_path(&image.thumbnail_path).unwrap();
                                    let handle = ui.ctx().load_texture(&image_key, image_data);
                                    *state.value_mut() = TextureState::Loaded(handle);
                                    ui.ctx().request_repaint();
                                    ui.label("Loading");
                                }
                                TextureState::Loaded(texture_handle) => {
                                    //need to show
                                    let mut size = texture_handle.size_vec2();
                                    clamp_to_width(&mut size, MAX_WIDTH);
                                    let image_button = ImageButton::new(texture_handle, size);
                                    if ui.add(image_button).clicked() {
                                        return Some(UserAction::ImageSelected(image.clone()));
                                    }
                                }
                            }
                        }
                        None => {
                            //We need to start a download
                            let image_handles = &self.image_selected_state.image_handles;
                            let path = &image.thumbnail_path;
                            if !path.exists() {
                                image_handles.insert(image_key.clone(), TextureState::Downloading);
                                let to_download = ToDownload {
                                    path: path.clone(),
                                    url: image.thumbnail_url.clone(),
                                    app_name: "Thumbnail".to_string(),
                                    image_type: *image_type,
                                };
                                let image_handles = image_handles.clone();
                                let image_key = image_key.clone();
                                self.rt.spawn_blocking(move || {
                                    block_on(crate::steamgriddb::download_to_download(
                                        &to_download,
                                    ))
                                    .unwrap();
                                    image_handles.insert(image_key, TextureState::Downloaded);
                                });
                            } else {
                                image_handles.insert(image_key.clone(), TextureState::Downloaded);
                            }
                        }
                    }
                }
            }
            _ => {
                ui.label("Finding possible images");
                ui.ctx().request_repaint();
            }
        }
        None
    }

    fn ensure_steam_users_loaded(&mut self) {
        self.image_selected_state
            .steam_users
            .get_or_insert_with(|| {
                get_shortcuts_paths(&self.settings.steam).expect("Should have steam user")
            });
    }

    pub(crate) fn render_ui_images(&mut self, ui: &mut egui::Ui) {
        self.ensure_games_loaded();
        self.ensure_steam_users_loaded();

        let mut action = UserAction::NoAction;
        ScrollArea::vertical()
            .stick_to_right()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.reset_style();
                action = self.render_ui_image_action(ui);
            });
        match action {
            UserAction::UserSelected(user) => {
                self.handle_user_selected(user);
            }
            UserAction::ShortcutSelected(shortcut) => {
                self.handle_shortcut_selected(shortcut, ui);
            }
            UserAction::ImageTypeSelected(image_type) => {
                self.handle_image_type_selected(image_type);
            }
            UserAction::ImageSelected(image) => {
                self.handle_image_selected(image);
            }
            UserAction::BackButton => {
                self.handle_back_button_action();
            }
            UserAction::GridIdChanged(grid_id) => {
                self.handle_grid_change(grid_id);
            }

            UserAction::NoAction => {}
            UserAction::CorrectGridId => {
                self.handle_correct_grid_request();
            }
        };
    }

    fn handle_correct_grid_request(&mut self) {
        let app_name = self
            .image_selected_state
            .selected_shortcut
            .as_ref()
            .map(|s| s.app_name.clone())
            .unwrap_or_default();
        let auth_key = self
            .settings
            .steamgrid_db
            .auth_key
            .clone()
            .unwrap_or_default();
        let client = steamgriddb_api::Client::new(&auth_key);
        let search_results = self.rt.block_on(client.search(&app_name));
        self.image_selected_state.possible_names = search_results.ok();
    }

    fn handle_grid_change(&mut self, grid_id: usize) {
        self.image_selected_state.grid_id = Some(grid_id);
        self.image_selected_state.possible_names = None;
        if let Some(auth_key) = &self.settings.steamgrid_db.auth_key {
            let client = steamgriddb_api::Client::new(auth_key);
            let mut cache = CachedSearch::new(&client);
            if let Some(shortcut) = &self.image_selected_state.selected_shortcut {
                cache.set_cache(shortcut.app_id, shortcut.app_name.clone(), grid_id);
                cache.save();
            }
        }
    }

    fn handle_user_selected(&mut self, user: SteamUsersInfo) {
        let state = &mut self.image_selected_state;
        state.user_shortcuts = Some(crate::steam::get_shortcuts_for_user(&user).shortcuts);
        state.steam_user = Some(user);
    }

    fn handle_image_type_selected(&mut self, image_type: ImageType) {
        let state = &mut self.image_selected_state;
        state.image_type_selected = Some(image_type);
        let (tx, rx) = watch::channel(FetcStatus::Fetching);
        self.image_selected_state.image_options = rx;
        let settings = self.settings.clone();
        if let Some(auth_key) = settings.steamgrid_db.auth_key {
            if let Some(grid_id) = self.image_selected_state.grid_id {
                let auth_key = auth_key;
                let image_type = image_type;
                self.rt.spawn_blocking(move || {
                    //Find somewhere else to put this
                    let _ = std::fs::create_dir_all(".thumbnails");
                    let thumbnails_folder = Path::new(".thumbnails");
                    let client = steamgriddb_api::Client::new(auth_key);
                    let query = get_query_type(false, &image_type);
                    let search_res = block_on(client.get_images_for_id(grid_id, &query));
                    if let Ok(possible_images) = search_res {
                        let mut result = vec![];
                        for possible_image in &possible_images {
                            let path = thumbnails_folder.join(format!("{}.png", possible_image.id));
                            result.push(PossibleImage {
                                thumbnail_path: path,
                                thumbnail_url: possible_image.thumb.clone(),
                                full_url: possible_image.url.clone(),
                                id: possible_image.id,
                            });
                            let _ = tx.send(FetcStatus::Fetched(result.clone()));
                        }
                    }
                });
            }
        };
    }

    fn handle_image_selected(&mut self, image: PossibleImage) {
        //We must have a user here
        let user = self.image_selected_state.steam_user.as_ref().unwrap();
        let selected_image_type = self
            .image_selected_state
            .image_type_selected
            .as_ref()
            .unwrap();
        let selected_image = self
            .image_selected_state
            .selected_shortcut
            .as_ref()
            .unwrap();
        let to = Path::new(&user.steam_user_data_folder)
            .join("config")
            .join("grid")
            .join(selected_image_type.file_name(selected_image.app_id));
        let app_name = selected_image.app_name.clone();
        let to_download = ToDownload {
            path: to,
            url: image.full_url.clone(),
            app_name,
            image_type: *selected_image_type,
        };
        self.rt.spawn_blocking(move || {
            let _ = block_on(crate::steamgriddb::download_to_download(&to_download));
        });

        {
            let image_ref = match selected_image_type {
                ImageType::Hero => &mut self.image_selected_state.hero_image,
                ImageType::Grid => &mut self.image_selected_state.grid_image,
                ImageType::WideGrid => &mut self.image_selected_state.wide_image,
                ImageType::Logo => &mut self.image_selected_state.logo_image,
                ImageType::BigPicture => &mut self.image_selected_state.wide_image,
                ImageType::Icon => &mut self.image_selected_state.icon_image,
            };
            let image_key = image.thumbnail_path.as_path().to_string_lossy().to_string();
            let texture_handle = self
                .image_selected_state
                .image_handles
                .get(&image_key)
                .unwrap();
            if let TextureState::Loaded(texture_handle) = texture_handle.value() {
                *image_ref = Some(texture_handle.clone());
            }
            self.image_selected_state.image_type_selected = None;
            self.image_selected_state.image_options = watch::channel(FetcStatus::NeedsFetched).1;
        }
        self.clear_loaded_images();
    }

    fn clear_loaded_images(&mut self) {
        self.image_selected_state.image_handles.clear();
    }

    fn handle_shortcut_selected(&mut self, shortcut: ShortcutOwned, ui: &mut egui::Ui) {
        let state = &mut self.image_selected_state;
        //We must have a user to make see this action;
        let user = state.steam_user.as_ref().unwrap();
        if let Some(auth_key) = &self.settings.steamgrid_db.auth_key {
            let client = steamgriddb_api::Client::new(auth_key);
            let search = CachedSearch::new(&client);
            state.grid_id = self
                .rt
                .block_on(search.search(shortcut.app_id, &shortcut.app_name))
                .ok()
                .flatten();
        }
        state.selected_shortcut = Some(shortcut.clone());
        let folder = Path::new(&user.steam_user_data_folder)
            .join("config")
            .join("grid");
        state.hero_image = get_image(ui, &shortcut, &folder, &ImageType::Hero);
        state.grid_image = get_image(ui, &shortcut, &folder, &ImageType::Grid);
        state.icon_image = get_image(ui, &shortcut, &folder, &ImageType::Icon);
        state.logo_image = get_image(ui, &shortcut, &folder, &ImageType::Logo);
        state.wide_image = get_image(ui, &shortcut, &folder, &ImageType::WideGrid);
        state.selected_shortcut = Some(shortcut);
    }

    fn handle_back_button_action(&mut self) {
        let state = &mut self.image_selected_state;
        if state.possible_names.is_some() {
            state.possible_names = None;
        } else if state.image_type_selected.is_some() {
            state.image_type_selected = None;
            state.image_handles.clear();
        } else if state.selected_shortcut.is_some() {
            state.selected_shortcut = None;
            state.hero_image = None;
            state.grid_image = None;
            state.icon_image = None;
            state.logo_image = None;
            state.wide_image = None;
        } else {
            state.user_shortcuts = None;
            state.steam_user = None;
        }
    }
}

fn render_possible_names(
    possible_names: &Vec<steamgriddb_api::search::SearchResult>,
    ui: &mut egui::Ui,
) -> Option<UserAction> {
    for possible in possible_names {
        if ui.button(&possible.name).clicked() {
            return Some(UserAction::GridIdChanged(possible.id));
        }
    }
    None
}

fn render_shortcut_images(ui: &mut egui::Ui, state: &ImageSelectState) -> Option<UserAction> {
    let mut grid_id_text = state.grid_id.map(|id| id.to_string()).unwrap_or_default();
    if ui.text_edit_singleline(&mut grid_id_text).changed() {
        if let Ok(grid_id) = grid_id_text.parse::<usize>() {
            return Some(UserAction::GridIdChanged(grid_id));
        }
    };
    if ui
        .button("Click here if the images are for a wrong game")
        .clicked()
    {
        return Some(UserAction::CorrectGridId);
    }
    for image_type in ImageType::all() {
        ui.label(image_type.name());
        let image_ref = get_image_ref(image_type, state);
        if render_thumbnail(ui, image_ref, &image_type) {
            return Some(UserAction::ImageTypeSelected(*image_type));
        }
    }
    None
}

fn render_user_select(state: &ImageSelectState, ui: &mut egui::Ui) -> UserAction {
    let users = state.steam_users.as_ref().unwrap();
    if users.len() == 1 {
        return UserAction::UserSelected(users[0].clone());
    }
    for user in users {
        if ui.button(&user.user_id).clicked() {
            return UserAction::UserSelected(user.clone());
        }
    }
    UserAction::NoAction
}

const MAX_WIDTH: f32 = 300.;

fn render_thumbnail(ui: &mut egui::Ui, image: &Option<egui::TextureHandle>, image_type:&ImageType) -> bool {
    if let Some(texture) = image {
        let mut size = texture.size_vec2();
        clamp_to_width(&mut size, MAX_WIDTH);
        let image_button = ImageButton::new(texture, size);
        let added =ui.add(image_button);
        match image_type{                
            ImageType::Icon => false,
            _=>  added.on_hover_text("Click to change image").clicked()
        }
    } else {
        match image_type{                
            ImageType::Icon => {
                ui.label("No icon found");
                false
            },
            _=> 
            ui.button("Pick an image").clicked(),
        }
    }
}

fn clamp_to_width(size: &mut egui::Vec2, max_width: f32) {
    let mut x = size.x;
    let mut y = size.y;
    if size.x > max_width {
        let ratio = size.y / size.x;
        x = max_width;
        y = x * ratio;
    }
    size.x = x;
    size.y = y;
}

fn get_image(
    ui: &mut egui::Ui,
    shortcut: &ShortcutOwned,
    folder: &std::path::Path,
    image_type: &ImageType,
) -> Option<egui::TextureHandle> {
    let file_name = ImageType::file_name(image_type, shortcut.app_id);
    let file_path = folder.join(file_name);
    let image = load_image_from_path(file_path.as_path()).map(|img_data| {
        ui.ctx()
            .load_texture(file_path.to_string_lossy().to_string(), img_data)
    });
    image
}

fn get_image_ref<'a>(
    image_type: &ImageType,
    state: &'a ImageSelectState,
) -> &'a Option<TextureHandle> {
    match image_type {
        ImageType::Hero => &state.hero_image,
        ImageType::Grid => &state.grid_image,
        ImageType::WideGrid => &state.wide_image,
        ImageType::Logo => &state.logo_image,
        ImageType::BigPicture => &state.wide_image,
        ImageType::Icon => &state.icon_image,
    }
}
