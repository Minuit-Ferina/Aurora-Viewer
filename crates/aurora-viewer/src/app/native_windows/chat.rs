use winit::event_loop::ActiveEventLoop;

use super::{Panel, View};
use crate::app::{App, Instant, NetCommand, UiActions, UiSound};
use crate::ui;

impl App {
    pub(in crate::app) fn apply_conversation_actions(
        &mut self,
        conversation_actions: Vec<ui::chat::ConvAction>,
        a: &mut UiActions,
        contact_actions: &mut Vec<ui::contacts::ContactsAction>,
        contacts_people: &mut Vec<ui::people::PeopleAction>,
    ) {
        let mut snoozes = Vec::new();
        for c in conversation_actions {
            match c {
                ui::chat::ConvAction::ColumnCollapsed(collapsed) => {
                    // (demo mode: this session only, never the file)
                    if self
                        .chat_ui
                        .column_collapsed_by_button(&mut self.settings.chat_column_collapsed, collapsed, self.demo)
                    {
                        self.settings.save();
                    }
                }
                ui::chat::ConvAction::Local(out) => a.chat = Some(out),
                ui::chat::ConvAction::Im { to, text } => a.ims.push((to, text)),
                ui::chat::ConvAction::SendSysinfo(to) => a.sysinfo.push(to),
                ui::chat::ConvAction::OfferTeleport(id) => a.offer_tp.push(id),
                ui::chat::ConvAction::ToggleBlock(id) => a.ctx_action = Some(ui::context::CtxAction::ToggleBlock(id)),
                ui::chat::ConvAction::Close(id) => {
                    self.world.conversation_closed(&id);
                    self.world.leave_session(id);
                    self.chat_ui.forget(id);
                    // closed from the list, another conversation may be shown
                    if self.chat_ui.selected == Some(id) {
                        self.chat_ui.selected = None;
                        self.chat_ui.show_profile = false;
                    }
                }
                ui::chat::ConvAction::Call(id) => a.ctx_action = Some(ui::context::CtxAction::Call(id)),
                ui::chat::ConvAction::HangUp => self.world.end_call(),
                ui::chat::ConvAction::ToggleMic => {
                    // toggle_mic, written out: the skin's icons stay borrowed
                    if self.speak_denied().is_none() {
                        self.mic_on = !self.mic_on;
                    }
                }
                ui::chat::ConvAction::AnswerCall { session, answer } => self.world.answer_call_invite(session, answer),
                ui::chat::ConvAction::PickPeople(conversation) => self.call_invite_ui.pick_for(conversation),
                ui::chat::ConvAction::AddPeople(conversation, ids) => self.call_invite_ui.add(&mut self.world, conversation, ids),
                ui::chat::ConvAction::SetVolume(id, v) => {
                    self.world.calls.set_volume(id, v);
                    // saved once the slider rests
                    self.volumes_save_at = Some(Instant::now() + std::time::Duration::from_millis(500));
                }
                ui::chat::ConvAction::Snooze(id) => snoozes.push(id),
                ui::chat::ConvAction::BlockGroupChat(id) => {
                    self.world.conversation_closed(&id);
                    self.world.set_group_chat_blocked(id, true);
                    self.chat_ui.forget(id);
                    self.chat_ui.selected = None;
                }
                ui::chat::ConvAction::OpenPeople(tab) => {
                    self.show_people_tab(tab);
                }
                ui::chat::ConvAction::Profile(id) => self.profile_ui.open(&mut self.world, id),
                ui::chat::ConvAction::Contacts(c) => contact_actions.push(c),
                ui::chat::ConvAction::People(pa) => contacts_people.push(pa),
                ui::chat::ConvAction::OpenIm(id) => self.world.start_text_conference(vec![id]),
                ui::chat::ConvAction::StartGroupChat(group) => self.world.start_group_chat(group, true),
                // LLIMModel::sendTypingState: to the resident, in
                // the session of the two of us (agent XOR other)
                ui::chat::ConvAction::Typing { to, typing } => a.typing.push(NetCommand::SendImDialog {
                    to,
                    dialog: if typing {
                        crate::world::IM_TYPING_START
                    } else {
                        crate::world::IM_TYPING_STOP
                    },
                    id: uuid::Uuid::from_u128(self.world.agent_id.as_u128() ^ to.as_u128()),
                    message: "typing".to_owned(),
                    bucket: Vec::new(),
                }),
                // the inventory's Share dialog confirms, then gives
                // (LLGiveInventory::doGiveInventoryItem)
                ui::chat::ConvAction::GiveItems { to, items } => {
                    self.inventory_ui.dialog = Some(ui::inventory::EditDialog::Share(items, vec![to]));
                }
                // shown and in front even when already open
                // (LLAvatarActions::share → showInstance), else the
                // menu entry seems to do nothing (#627)
                ui::chat::ConvAction::OpenInventory => {
                    ui::widgets::open_floater(&self.egui_ctx, "inventory", &mut self.panels.inventory);
                    self.egui_ctx
                        .move_to_top(egui::LayerId::new(egui::Order::Middle, egui::Id::unique("inventory")));
                }
                ui::chat::ConvAction::SetOutputDevice(device) => {
                    self.settings.audio.output_device = device;
                    self.settings.save();
                }
                ui::chat::ConvAction::ListOutputDevices => {
                    self.device_lister.request();
                }
                // done by the window before it returns
                ui::chat::ConvAction::PopOut(_) | ui::chat::ConvAction::DockBack(_) | ui::chat::ConvAction::PopOutAt(..) => {}
            }
        }
        for group in snoozes {
            Self::ask_or_snooze(&self.settings, self.demo, &mut self.world, &mut self.chat_ui, group);
        }
    }

    pub(in crate::app) fn apply_people_actions(&mut self, people: impl IntoIterator<Item = ui::people::PeopleAction>, a: &mut UiActions) {
        for pa in people {
            match pa {
                ui::people::PeopleAction::OpenIm(id) => {
                    self.open_conversation_to_type(id);
                }
                ui::people::PeopleAction::OfferTeleport(id) => a.offer_tp.push(id),
                ui::people::PeopleAction::Profile(id) => self.profile_ui.open(&mut self.world, id),
                ui::people::PeopleAction::RosterProfile(id) => {
                    self.profile_ui.open_in(&mut self.world, crate::world::names::NameScope::Roster, id);
                }
                ui::people::PeopleAction::ZoomRefused { name, why } => {
                    let meters = |d: f32| aurora_i18n::number(f64::from(d), 0);
                    let text = match why {
                        ui::people::ZoomRefusal::BeyondDrawDistance { range, draw_distance } => aurora_i18n::tr!(
                            "radar-zoom-beyond-draw-distance",
                            name = name,
                            distance = meters(range),
                            draw = meters(draw_distance)
                        ),
                        ui::people::ZoomRefusal::Unplaced => aurora_i18n::tr!("radar-zoom-unplaced", name = name),
                    };
                    self.world.system_message(text);
                }
                ui::people::PeopleAction::GroupChat(id) => {
                    self.world.ui_sounds.push(UiSound::StartIm);
                    self.world.start_group_chat(id, true);
                    self.show_conversations();
                }
                ui::people::PeopleAction::GroupNotices(id, on) => self.world.groups.set_accept_notices(id, on),
                ui::people::PeopleAction::ModifyRights { friend, rights, grant } => {
                    self.contacts_ui.ask_modify_rights(friend, rights, grant)
                }
                ui::people::PeopleAction::PickFriend => self.contacts_ui.pick_new_friend(),
                ui::people::PeopleAction::ConversationLog => {
                    ui::widgets::toggle_floater(&self.egui_ctx, ui::conversation_log::WINDOW, &mut self.panels.conversation_log);
                }
                ui::people::PeopleAction::Unblock(id, name) => self.world.unblock(id, &name, 0),
                ui::people::PeopleAction::BlockByName(name) => {
                    if let Err(e) = self.world.block(uuid::Uuid::nil(), &name, crate::world::mutes::MuteType::ByName) {
                        self.world.system_message(e);
                    }
                }
                ui::people::PeopleAction::BlockFlag { id, name, kind, flag, on } => {
                    if on {
                        if let Err(e) = self.world.block_flags(id, &name, kind, flag) {
                            self.world.system_message(e);
                        }
                    } else {
                        self.world.unblock(id, &name, flag);
                    }
                }
                ui::people::PeopleAction::SearchGroups => {
                    self.panels.search = true;
                    self.search_ui.open_tab(crate::world::search::Tab::Groups);
                }
                ui::people::PeopleAction::GroupData(group) => self.world.group_data.open(group, true),
                ui::people::PeopleAction::GroupTitle { group, role } => {
                    if !self.world.rlva.has("setgroup") {
                        self.world.group_data.set_title(group, role);
                    }
                }
                ui::people::PeopleAction::GroupFlags { group, notices, profile } => self.world.groups.set_flags(group, notices, profile),
                ui::people::PeopleAction::InviteToGroup(group) => self.group_profile_ui.open_with_invite(&mut self.world, group),
            }
        }
    }

    pub(in crate::app) fn pump_native_windows(&mut self, event_loop: &dyn ActiveEventLoop) {
        let panel = Panel::Conversations;
        let allowed = self.in_world() && !ui::widgets::chat_windows_hidden(&self.egui_ctx);
        self.native_windows.reconcile(panel, allowed && self.panels.chat);
        self.native_windows
            .reconcile_popouts(|conversation| allowed && ui::chat::popout_available(&self.chat_ui, conversation));
        self.reconcile_desktop_inventory();
        self.reconcile_desktop_build();
        self.reconcile_desktop_scripts();
        self.reconcile_desktop_notecards();
        self.reconcile_desktop_info();
        self.reconcile_desktop_world_map();
        self.reconcile_desktop_people();
        self.reconcile_desktop_places();
        let Some(gfx) = &mut self.gfx else { return };
        let drawable = self.native_windows.next(event_loop, &mut gfx.renderer, &self.egui_ctx);
        self.native_windows.update_chat_presence(&self.egui_ctx);
        self.native_windows.update_notecard_presence(&self.egui_ctx);
        if self.native_windows.take_open_failure() {
            self.world.system_message(aurora_i18n::tr!("native-windows-open-failed").to_owned());
        }
        let Some(drawable) = drawable else { return };
        match drawable {
            Panel::Inventory(id) => {
                self.paint_desktop_inventory(event_loop, id);
                return;
            }
            Panel::Build => {
                self.paint_desktop_build(event_loop);
                return;
            }
            Panel::About | Panel::GridStatus | Panel::Bumps | Panel::GroupTitles | Panel::RegionTracker | Panel::AvatarRenderSettings => {
                self.paint_desktop_info(event_loop, drawable);
                return;
            }
            Panel::Places => {
                self.paint_desktop_places(event_loop);
                return;
            }
            Panel::People => {
                self.paint_desktop_people(event_loop);
                return;
            }
            Panel::WorldMap => {
                self.paint_desktop_world_map(event_loop);
                return;
            }
            Panel::Notecard(key) => {
                self.paint_desktop_notecard(event_loop, key);
                return;
            }
            Panel::Script(object, item) => {
                self.paint_desktop_script(event_loop, aurora_net::scripts::ScriptTarget { object, item });
                return;
            }
            _ => {}
        }
        let mut accepted_close = false;
        let mut close_all = false;
        let mut actions = Vec::new();
        let mut snooze = None;
        let modal = self.exit_prompt.open;
        let ctx = self.egui_ctx.clone();
        let friends_before = (drawable == panel).then(|| self.settings.friends_list.clone());
        let second_life = self.second_life();
        let Some(gfx) = &mut self.gfx else { return };
        self.native_windows.paint(
            View {
                event_loop,
                renderer: &mut gfx.renderer,
                root: gfx.window.as_ref(),
                context: &ctx,
                palette: &self.palette,
                shadow: &mut self.egui_shadow,
            },
            drawable,
            |ui, close, _, caption| {
                close_all |= !modal && ui::close_key::desktop_close_all(ui.ctx());
                if modal {
                    ui.disable();
                    caption.disable();
                }
                if let Panel::Conversation(conversation) = drawable {
                    let close = !modal && (close || ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)));
                    actions.extend(ui::chat::desktop_popout(
                        ui,
                        caption,
                        &self.palette,
                        &self.skin.icons,
                        &mut self.emoji,
                        &self.avatar_pics,
                        &self.ui_images,
                        &mut self.world,
                        &self.gesture_state.manager,
                        &mut self.chat_ui,
                        &mut self.panels.chat,
                        conversation,
                        close,
                    ));
                    return close;
                }
                if !modal && close {
                    accepted_close = true;
                    return true;
                }
                let voice = self.voice.levels(self.world.agent_id);
                let radar = ui::people::RadarData {
                    radar: &self.radar,
                    settings: &mut self.settings.radar,
                    voice: &voice,
                    draw_distance: self.settings.draw_distance,
                    second_life,
                    now: self.radar_clock.elapsed().as_secs_f64(),
                    pics: &self.avatar_pics,
                };
                let friends =
                    (!self.settings.classic_people && !(self.demo && crate::demo::people::legacy())).then_some(ui::people::FriendsPane {
                        st: &mut self.contacts_people_ui,
                        data: &radar,
                        settings: &mut self.settings.friends_list,
                        images: &self.ui_images,
                    });
                actions.extend(ui::chat::show_placed(
                    &ctx,
                    &self.palette,
                    &self.skin.icons,
                    &mut self.emoji,
                    &self.avatar_pics,
                    &self.ui_images,
                    &mut self.world,
                    &self.gesture_state.manager,
                    &mut self.chat_ui,
                    &mut self.contacts_ui,
                    &mut self.settings.contacts,
                    self.settings.show_contacts_tab,
                    friends,
                    &mut self.panels.chat,
                    ui::chat::ContainerHost::Desktop { ui, caption },
                ));
                snooze = ui::chat::snooze::dialog(&ctx, &self.palette, &self.world, &mut self.chat_ui).or(snooze);
                !self.panels.chat
            },
        );
        if close_all {
            self.native_windows.request_close_all();
            ui::close_key::queue_root_close_all(&ctx);
        }
        if accepted_close {
            self.panels.chat = false;
        }
        if let Some((group, minutes)) = snooze {
            Self::snooze_group_chat(&mut self.world, &mut self.chat_ui, group, minutes.saturating_mul(60));
        }
        if let Some(before) = friends_before
            && self.settings.friends_list != before
        {
            self.settings.save();
        }
        if !actions.is_empty() {
            let mut a = UiActions::default();
            let mut contacts = Vec::new();
            let mut people = Vec::new();
            self.apply_conversation_actions(actions, &mut a, &mut contacts, &mut people);
            for contact in contacts {
                self.on_contacts_action(contact, &mut a);
            }
            self.apply_people_actions(people, &mut a);
            self.handle_actions(a, event_loop);
        }
    }
}
