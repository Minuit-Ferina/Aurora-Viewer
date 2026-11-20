//! The Group titles window (`ui/group_titles.rs`) and the title worn by
//! region (`world/region_titles.rs`): the account's rules read at login,
//! every group's titles asked while the window is open, the arrival's title
//! sent once per region, and the window's actions.
use super::*;
use crate::ui::group_titles::{TitlesAction, View};
use crate::world::region_titles::RegionTitles;
use uuid::Uuid;

impl App {
    /// FSGroupTitleRegionMgr::loadFromDisk at login (LLStartup): the
    /// account's rules on this grid. The demo keeps its own in memory.
    pub(super) fn load_region_titles(&mut self, agent: Uuid) {
        self.world.region_titles = if self.demo {
            RegionTitles::in_memory(crate::demo::group_region::rules())
        } else {
            let grid = crate::world::chat_log::grid_name(&self.settings.grid, &self.settings.custom_login_uri, false);
            RegionTitles::load(crate::settings::account_dir(&agent).join(crate::world::region_titles::file_name(&grid)))
        };
    }

    /// Every frame: the titles the open window lists, the arrival's title
    /// and the typed region's lookup.
    pub(super) fn group_titles_tick(&mut self) {
        if self.panels.group_titles {
            let groups: Vec<Uuid> = self.world.groups.groups.iter().map(|g| g.id).collect();
            self.world.region_titles.request_titles(groups);
        }
        for cmd in self.world.region_title_tick(Instant::now()) {
            self.send(cmd);
        }
    }

    /// The Group titles window, while open.
    pub(super) fn group_titles_window(&mut self, ctx: &egui::Context, p: &crate::theme::Palette) {
        let panel = native_windows::Panel::GroupTitles;
        if self.native_windows.is_native(panel) {
            return;
        }
        let region = Self::group_titles_region(&self.world);
        let view = Self::group_titles_view(&self.world, region.as_deref());
        let mut desktop = ui::widgets::DesktopPanels::default();
        let actions = self.group_titles_ui.show(
            ctx,
            p,
            &view,
            &mut self.panels.group_titles,
            ui::widgets::PanelHost::Viewer(&mut desktop),
        );
        if !desktop.requested.is_empty() {
            self.native_windows
                .detach(panel, aurora_i18n::tr!("toolbar-command-group-titles").to_owned());
        }
        for action in actions {
            self.on_group_titles_action(action, region.as_deref());
        }
    }

    pub(super) fn group_titles_region(world: &World) -> Option<String> {
        let region = world.region_name();
        (!region.is_empty() && !world.rlva.has("showloc")).then_some(region)
    }

    pub(super) fn group_titles_view<'a>(world: &'a World, region: Option<&'a str>) -> View<'a> {
        View {
            groups: &world.groups.groups,
            active: world.groups.active,
            titles: &world.region_titles,
            region,
        }
    }

    pub(super) fn on_group_titles_action(&mut self, action: TitlesAction, region: Option<&str>) {
        let titles = &mut self.world.region_titles;
        match action {
            // FSFloaterGroupTitles::activateGroupTitle
            TitlesAction::Activate { group, role } => {
                if !group.is_nil() {
                    self.send(NetCommand::GroupTitleUpdate { group, role });
                }
                if self.world.groups.active != group {
                    self.send(NetCommand::ActivateGroup(group));
                }
            }
            TitlesAction::Info(group) => ui::group_profile::request_open(&self.egui_ctx, group),
            TitlesAction::Refresh => {
                let groups: Vec<Uuid> = self.world.groups.groups.iter().map(|g| g.id).collect();
                titles.refresh_titles(groups);
            }
            TitlesAction::AssignHere(target) => {
                if let Some(region) = region {
                    titles.lookup_result = None;
                    titles.assign(region, target);
                }
            }
            TitlesAction::LookUp(name, target) => {
                let known = self.world.map_region_named(&name);
                self.world.region_titles.look_up(&name, target, known, Instant::now());
            }
            TitlesAction::Reassign(region, target) => titles.assign(&region, target),
            TitlesAction::Remove(region) => titles.remove(&region),
            TitlesAction::RemoveTitle(group, role) => titles.remove_title(group, role),
            TitlesAction::UseDefault(on) => titles.set_use_default(on),
            TitlesAction::SetDefault(target) => titles.set_default(target),
        }
    }
}
