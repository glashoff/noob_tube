//! Drawing a player as a person rather than as a capsule.
//!
//! A player entity arrives from the server carrying [`PlayerState`] and [`Aim`] and nothing
//! visible. This hangs a character model under it and picks the animation that matches what the
//! simulation says the player is doing — standing, walking, running, crouching or in the air, and
//! in which of eight directions.
//!
//! **Two kits, and the difference between them is licensing rather than taste.** The Mixamo
//! soldier is what the game is built around and what `webgame` used before it: a proper 8-way
//! locomotion set, aiming idles, deaths. It may be used and not redistributed, so it is not in
//! this repository and `tools/setup-assets` is what brings it in. Without it the game falls back
//! to the Quaternius character, which is CC0 and committed — a whole figure rather than a capsule,
//! but with forward locomotion only. Nothing here may *require* an asset that a fresh checkout
//! does not have, which is the same rule the vehicle model follows.
//!
//! **The model and the clips come from different files**, and that only works because they share a
//! skeleton exactly. Bevy binds an animation to a bone by the *path* of names from the scene root
//! down, so `RootNode/mixamorig:Hips/mixamorig:Spine` has to be spelt the same in both. It is —
//! measured, not hoped for:
//!
//! ```text
//! tools/glb rigs assets/characters/swat.glb assets/anims/idle.glb
//! ```
//!
//! reports all 70 bone paths shared, the only three unmatched being the character's own mesh
//! nodes, which no clip has a curve for. That check is the whole reason this is a handful of
//! systems rather than a retargeting project — see the README under "Character assets".

use bevy::animation::{AnimatedBy, AnimationTargetId};
use bevy::app::AnimationSystems;
use bevy::transform::TransformSystems;
use bevy::gltf::Gltf;
use bevy::prelude::*;
use core::time::Duration;
use lightyear::prelude::*;
use noob_tube_shared::movement::CAPSULE_HEIGHT;
use noob_tube_shared::player::{Aim, Player, PlayerState};

/// Everything that makes one set of character assets usable, as a table entry.
///
/// Deliberately not a trait and not two code paths. The two kits differ in what they are *called*
/// and how tall they are; what is done with them is the same, and a second soldier is a second
/// entry rather than a second pipeline.
struct Kit {
    /// The body, under the asset directory.
    body: &'static str,
    /// Where the clips come from — one file each, or all in one library.
    clips: Clips,
    /// How tall the body is in its own file, standing, in metres.
    ///
    /// Measured rather than guessed; `tools/glb nodes <file>` reads it back out of the mesh's own
    /// bounds. Replacing a body means measuring this again, because everything scales by it.
    height: f32,
    /// The ground speeds the walk, run and crouch-walk cycles were authored for, in metres per
    /// second.
    ///
    /// This is what foot lock needs: play a stride at a speed it was not made for and the feet
    /// skate along the ground. Playing it at `actual / authored` puts the stride back in step.
    ///
    /// Measured, not guessed — `tools/glb animations <clip>` prints it from how far the hips
    /// travel over the clip. The soldier's numbers come out at 1.84, 4.61 and 1.96, which are
    /// `webgame`'s 4.606 and 1.956 to three figures; the fallback kit's at 0.97, 5.36 and 0.75,
    /// read out of the download's `_RM` variant since the committed one has root motion stripped.
    paces: Paces,
    /// The bone a locomotion clip carries the figure forward on, if its clips carry root motion.
    ///
    /// `None` for a pack that was exported with it stripped. Which is which is a measurement, not
    /// a guess — `tools/glb animations <clip>` prints how far a clip travels, and prints nothing
    /// for one that does not.
    ///
    /// Only ever one bone. Every one of the soldier's 49 clips translates `mixamorig:Hips` and
    /// nothing else at all: `walk_forward` moves it 1.84 m along Z, `run_left` 2.30 m along X, and
    /// no other bone has a translation track in any of them.
    root_motion: Option<&'static str>,
}

/// The three locomotion cycles' authored speeds, in metres per second.
struct Paces {
    walk: f32,
    run: f32,
    crouch: f32,
}

/// How a kit's animations are packaged.
enum Clips {
    /// One glTF per clip, named by its file: Mixamo's shape, and `webgame`'s.
    ///
    /// The clip inside each carries no useful name of its own — every one of the 49 is called
    /// `mixamo.com` — so the file name is the name, which is also why `tools/setup-assets` keeps
    /// Mixamo's own spelling.
    PerFile { directory: &'static str },
    /// All of them in one file, looked up by the name they carry: Quaternius' shape.
    Library { file: &'static str },
}

/// The Mixamo soldier, and the 49 clips `tools/setup-assets` brings in beside it.
const SWAT: Kit = Kit {
    body: "characters/swat.glb",
    clips: Clips::PerFile { directory: "anims" },
    height: 1.78,
    paces: Paces { walk: 1.84, run: 4.61, crouch: 1.96 },
    root_motion: Some("mixamorig:Hips"),
};

/// The CC0 fallback, so that a checkout with no Mixamo assets still has a person in it.
const MANNEQUIN: Kit = Kit {
    body: "characters/Superhero_Male_FullBody.gltf",
    clips: Clips::Library { file: "animations/universal_animation_library_1.glb" },
    height: 1.810,
    paces: Paces { walk: 0.97, run: 5.36, crouch: 0.75 },
    // The committed variant of this pack is the one with root motion already stripped, which is
    // the whole reason it is the one that is in. Measured: its clips travel 0.00 m.
    root_motion: None,
};

/// Half a turn, because glTF says a model faces +Z and this game says forward is −Z.
///
/// Confirmed against both files rather than taken from the specification: the Quaternius eyes sit
/// at z 0.04 to 0.09, and the soldier's toe bone is 10 cm in front of its ankle on the same side.
const MODEL_YAW: f32 = core::f32::consts::PI;

/// How long a change of animation takes to blend.
///
/// Long enough that starting to walk is not a snap, short enough that it is not a slide. A tenth of
/// a second is about one frame of a run cycle at these speeds.
const BLEND: Duration = Duration::from_millis(120);

/// Below this, a player is standing still rather than walking slowly.
///
/// A shade under a tenth of a metre a second. Lower and a player who has stopped keeps shuffling on
/// the last of their deceleration; higher and the first step of a walk is a slide.
const STILL: f32 = 0.08;

/// How far a clip may be sped up or slowed down before it is left alone to skate a little.
///
/// A stride played at three times its pace does not read as running fast, it reads as broken. With
/// the soldier's clips nothing comes near this — full speed is 1.19x its run — and it is the
/// fallback kit that runs into it, whose crouch is authored for 0.75 m/s against this game's 2.6.
const RATE: (f32, f32) = (0.5, 2.0);

/// What a player's body is doing, as coarsely as the clips distinguish it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gait {
    Idle,
    Walk,
    Run,
    CrouchIdle,
    CrouchWalk,
    Airborne,
}

/// Which way a player is travelling relative to the way they are facing.
///
/// Eight, because that is what the soldier's pack has and because it is what a shooter needs: a
/// player strafing right while looking at you is the commonest thing on the screen, and running
/// forward while doing it is a different silhouette entirely. The fallback kit has none of these
/// and reads every one of them as forward — which is exactly why it is the fallback.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Facing {
    Forward,
    ForwardLeft,
    Left,
    BackwardLeft,
    Backward,
    BackwardRight,
    Right,
    ForwardRight,
}

impl Facing {
    /// The eight, in the order the angle below indexes them.
    const ALL: [Facing; 8] = [
        Facing::Forward,
        Facing::ForwardLeft,
        Facing::Left,
        Facing::BackwardLeft,
        Facing::Backward,
        Facing::BackwardRight,
        Facing::Right,
        Facing::ForwardRight,
    ];

    /// Which of the eight a direction of travel is nearest, seen from behind the player.
    ///
    /// `travel` is in world space and `yaw` is where the player is looking, so the first thing
    /// this does is take the travel into the player's own frame — a body strafes relative to
    /// where its head is pointed, not relative to the world.
    ///
    /// Forward is −Z, as everywhere else in this game, and the eighth-turns are centred on each
    /// direction rather than starting at it: adding half a step before rounding is what makes
    /// "almost exactly forward" round to forward rather than to whichever neighbour it leans on.
    fn of(travel: Vec3, yaw: f32) -> Facing {
        let local = Quat::from_rotation_y(-yaw) * travel;
        let angle = f32::atan2(local.x, -local.z);
        let step = core::f32::consts::TAU / 8.0;
        let index = ((angle / step).round() as i32).rem_euclid(8) as usize;
        // atan2(x, -z) grows towards +X, which is the player's right, so the index counts
        // clockwise from forward while `ALL` is written anticlockwise. Reading it backwards is
        // what puts a step to the right on the right foot.
        Facing::ALL[(8 - index) % 8]
    }

    /// The suffix Mixamo spells this direction with.
    fn suffix(self) -> &'static str {
        match self {
            Facing::Forward => "forward",
            Facing::ForwardLeft => "forward_left",
            Facing::Left => "left",
            Facing::BackwardLeft => "backward_left",
            Facing::Backward => "backward",
            Facing::BackwardRight => "backward_right",
            Facing::Right => "right",
            Facing::ForwardRight => "forward_right",
        }
    }
}

/// One clip a kit can be asked for.
type Move = (Gait, Facing);

/// Every clip this game can ask for. Fewer than either pack holds, and deliberately: a name that is
/// not here is one nothing in the simulation can reach.
fn every_move() -> Vec<Move> {
    let mut out = Vec::new();
    for gait in [Gait::Idle, Gait::CrouchIdle, Gait::Airborne] {
        out.push((gait, Facing::Forward));
    }
    for gait in [Gait::Walk, Gait::Run, Gait::CrouchWalk] {
        for facing in Facing::ALL {
            out.push((gait, facing));
        }
    }
    out
}

impl Kit {
    /// What this kit calls one movement, or `None` if it has nothing for it.
    fn clip(&self, (gait, facing): Move) -> Option<String> {
        Some(match self.clips {
            Clips::PerFile { .. } => match gait {
                Gait::Idle => "idle".to_string(),
                Gait::CrouchIdle => "idle_crouching".to_string(),
                Gait::Airborne => "jump_loop".to_string(),
                Gait::Walk => format!("walk_{}", facing.suffix()),
                Gait::Run => format!("run_{}", facing.suffix()),
                Gait::CrouchWalk => format!("walk_crouching_{}", facing.suffix()),
            },
            // Forward locomotion only, so every direction reads as forward. Asking for the same
            // clip eight times is fine — the graph holds one node per *movement*, and the eight
            // share it.
            Clips::Library { .. } => match gait {
                Gait::Idle => "Idle_Loop",
                Gait::CrouchIdle => "Crouch_Idle_Loop",
                Gait::Airborne => "Jump_Loop",
                Gait::Walk => "Walk_Loop",
                Gait::Run => "Jog_Fwd_Loop",
                Gait::CrouchWalk => "Crouch_Fwd_Loop",
            }
            .to_string(),
        })
    }

    /// The speed the cycle for this gait was authored for, or `None` for one that does not travel.
    fn pace(&self, gait: Gait) -> Option<f32> {
        match gait {
            Gait::Walk => Some(self.paces.walk),
            Gait::Run => Some(self.paces.run),
            Gait::CrouchWalk => Some(self.paces.crouch),
            Gait::Idle | Gait::CrouchIdle | Gait::Airborne => None,
        }
    }

    /// Where the walk gives way to the run, in metres per second.
    ///
    /// The geometric mean of the two cycles' authored speeds, which is the crossover that keeps
    /// the *ratio* either is stretched by as small as it can be: at it, both are stretched alike.
    /// Derived rather than chosen, so a kit whose clips are paced differently gets the right
    /// crossover without anybody having to notice.
    fn trot(&self) -> f32 {
        (self.paces.walk * self.paces.run).sqrt()
    }

    /// What the model is scaled by so that it is exactly as tall as the collision capsule.
    ///
    /// Not decoration. The capsule *is* the hitbox — a shot is tested against it and nothing else
    /// — so a body drawn at its own height inside a shorter capsule would put a head where a
    /// player can aim and no raycast can reach. That mistake has been made here once already, with
    /// the placeholder head box this replaces, and it is invisible until somebody complains that
    /// head shots do not register.
    ///
    /// What this does *not* fix is width: a character with an arm out reaches past a 35 cm capsule
    /// and no uniform scale changes that. It is the honest cost of a real model over a capsule,
    /// and the argument for the per-bone hitboxes under "Still to settle" in the README.
    fn scale(&self) -> f32 {
        CAPSULE_HEIGHT / self.height
    }

    /// Whether this kit's files are on disk.
    ///
    /// From the filesystem rather than from the asset server, which would answer asynchronously —
    /// some frames after the first player has already arrived and needed a body. The same choice
    /// and the same reason as the vehicle model's.
    fn present(&self) -> bool {
        let assets = std::path::Path::new(crate::ASSETS);
        if !assets.join(self.body).exists() {
            return false;
        }
        match self.clips {
            Clips::PerFile { directory } => assets.join(directory).join("idle.glb").exists(),
            Clips::Library { file } => assets.join(file).exists(),
        }
    }
}

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_the_character)
            .add_systems(
                Update,
                (
                    name_the_clips.run_if(not(resource_exists::<Moves>)),
                    give_bodies,
                    // Both need the graph, and a kit whose clips live in one library does not have
                    // one until that library has finished loading. Gating them is what keeps that
                    // from being a panic on the first frame.
                    (wire_up_the_skeleton, choose_the_motion).run_if(resource_exists::<Moves>),
                )
                    .chain()
                    // The same reason `remote_players` waits: interpolation writes the smoothed
                    // `PlayerState` in Update, and choosing a clip from last frame's sample would
                    // add a frame of lag to a decision that is already a frame behind.
                    .after(InterpolationSystems::All),
            )
            .add_systems(
                PostUpdate,
                stay_where_the_simulation_put_them
                    // After the animation has written the pose and before anything reads it. A
                    // clip's own travel has to be undone every frame it is evaluated, and there is
                    // no earlier point at which it exists to be undone.
                    .after(AnimationSystems)
                    .before(TransformSystems::Propagate),
            );
    }
}

/// Which kit is in use, and the handles that were asked for from it.
#[derive(Resource)]
struct CharacterAssets {
    kit: &'static Kit,
    body: Handle<WorldAsset>,
    /// Only for a kit whose clips share one file, which has to be loaded before the names in it
    /// can be looked up.
    library: Option<Handle<Gltf>>,
}

/// The animation graph, and where each movement sits in it.
#[derive(Resource)]
struct Moves {
    graph: Handle<AnimationGraph>,
    nodes: Vec<(Move, AnimationNodeIndex)>,
    /// One clip, kept only so the wiring can check itself against it. See
    /// [`wire_up_the_skeleton`], which counts how many of the bones it just labelled this clip
    /// actually has a curve for — a number that is either "most of them" or "none".
    probe: Handle<AnimationClip>,
}

impl Moves {
    fn node(&self, wanted: Move) -> Option<AnimationNodeIndex> {
        self.nodes.iter().find(|(which, _)| *which == wanted).map(|(_, node)| *node)
    }
}

/// The child entity holding the model, so that the player entity's own transform stays the pose the
/// simulation writes.
#[derive(Component)]
struct CharacterBody;

/// A player that has just been replicated to us and has nothing to be seen as yet.
type JustArrived = (With<client::Remote>, Without<Predicted>, Added<PlayerState>);

/// A body whose model has spawned but whose skeleton has not been given its plumbing.
type NotWiredYet = (With<CharacterBody>, Without<Wired>);

/// On the entity holding the [`AnimationPlayer`], naming the player it belongs to.
///
/// The link has to be stored because the skeleton arrives asynchronously and several levels down:
/// by the time there is an `AnimationPlayer` to drive, the player entity is four parents away.
#[derive(Component)]
struct Animates(Entity);

/// Startup: picks the kit that is on disk and asks for it.
///
/// The soldier wins where it is present, because it is what the game is built around; the CC0
/// figure is what a checkout without it still gets, and it is a whole person rather than a capsule.
/// Said out loud at startup, because "which character am I looking at" is otherwise a thing to
/// deduce from the picture.
fn load_the_character(assets: Res<AssetServer>, mut commands: Commands) {
    let kit = if SWAT.present() {
        info!("character: the Mixamo soldier, with 8-way locomotion");
        &SWAT
    } else {
        info!(
            "character: the CC0 fallback, forward locomotion only — `tools/setup-assets` brings \
             in the soldier",
        );
        &MANNEQUIN
    };

    let body = assets.load(GltfAssetLabel::Scene(0).from_asset(kit.body));
    let mut library = None;
    match kit.clips {
        // One file per clip needs no lookup, so the graph can be built here and now.
        Clips::PerFile { directory } => {
            let mut graph = AnimationGraph::new();
            let mut nodes = Vec::new();
            let mut probe = None;
            for wanted in every_move() {
                let Some(name) = kit.clip(wanted) else {
                    continue;
                };
                let clip: Handle<AnimationClip> =
                    assets.load(GltfAssetLabel::Animation(0).from_asset(format!("{directory}/{name}.glb")));
                probe.get_or_insert_with(|| clip.clone());
                nodes.push((wanted, graph.add_clip(clip, 1.0, graph.root)));
            }
            let probe = probe.expect("every movement names a clip");
            info!("{} clips ready", nodes.len());
            commands.queue(move |world: &mut World| {
                let graph = world.resource_mut::<Assets<AnimationGraph>>().add(graph);
                world.insert_resource(Moves { graph, nodes, probe });
            });
        }
        // Nothing to build yet: the names in a library cannot be looked up until it has loaded.
        Clips::Library { file } => library = Some(assets.load::<Gltf>(file)),
    }
    commands.insert_resource(CharacterAssets { kit, body, library });
}

/// Update, until it succeeds: builds the graph for a kit whose clips share one file.
///
/// By name rather than by index. The library holds 43 clips and glTF gives them no order worth
/// relying on; `Idle_Loop` will still be `Idle_Loop` after the pack is updated, where clip 9 will
/// not be.
fn name_the_clips(
    assets: Res<CharacterAssets>,
    library: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut commands: Commands,
) {
    let Some(handle) = assets.library.as_ref() else {
        return;
    };
    let Some(gltf) = library.get(handle) else {
        return;
    };
    let mut graph = AnimationGraph::new();
    let mut nodes = Vec::new();
    let mut probe = None;
    // The same name several times over, because a forward-only kit answers every direction with
    // one clip. Adding it once and sharing the node is what keeps eight entries from being eight
    // copies of the same curves.
    let mut added: Vec<(String, AnimationNodeIndex)> = Vec::new();
    for wanted in every_move() {
        let Some(name) = assets.kit.clip(wanted) else {
            continue;
        };
        let node = match added.iter().find(|(seen, _)| *seen == name) {
            Some((_, node)) => *node,
            None => {
                let Some(clip) = gltf.named_animations.get(name.as_str()) else {
                    error!("the clip library has nothing called {name}");
                    continue;
                };
                probe.get_or_insert_with(|| clip.clone());
                let node = graph.add_clip(clip.clone(), 1.0, graph.root);
                added.push((name, node));
                node
            }
        };
        nodes.push((wanted, node));
    }
    let Some(probe) = probe else {
        error!("the clip library held none of the clips this game asks for");
        return;
    };
    info!("{} clips ready, covering {} movements", added.len(), nodes.len());
    commands.insert_resource(Moves { graph: graphs.add(graph), nodes, probe });
}

/// Update: hangs a model under a player that has just arrived.
///
/// `Without<Predicted>` leaves our own player out: the camera sits inside it, and a body there
/// would fill the screen. It is a child rather than the player entity itself because the two want
/// different transforms — the player entity carries the pose the simulation writes, in world units
/// and facing −Z, and this carries the half turn and the scale that make a glTF model agree.
fn give_bodies(
    arrived: Query<(Entity, &Player), JustArrived>,
    assets: Res<CharacterAssets>,
    mut commands: Commands,
) {
    for (entity, player) in arrived.iter() {
        commands
            .entity(entity)
            .insert((Name::from(format!("Remote player {}", player.peer)), Transform::default()))
            .with_child((
                Name::from("Body"),
                CharacterBody,
                WorldAssetRoot(assets.body.clone()),
                Transform::from_rotation(Quat::from_rotation_y(MODEL_YAW))
                    .with_scale(Vec3::splat(assets.kit.scale())),
            ));
        info!("drawing player {}", player.peer);
    }
}

/// The bone that carries a clip's own travel, and where it sits when nothing is driving it.
///
/// Only the two horizontal components are kept, because only those are wrong. See
/// [`stay_where_the_simulation_put_them`].
#[derive(Component)]
struct Planted(Vec2);

/// PostUpdate: takes the clip's own travel back out of the pose.
///
/// A Mixamo locomotion clip carries the figure forward inside the animation — `walk_forward` moves
/// the hips 1.84 m over its one second. World position here is the server's to decide, so that
/// displacement has to go, or a walking player slides away from their own position and every shot
/// at them misses a body that is not where it is drawn.
///
/// **X and Z are pinned; Y is left alone.** The vertical is the hip bob, 6 cm of it in the walk,
/// and a figure without it does not put its weight on its feet — it glides. That is the whole of
/// the distinction, and getting it backwards is the mistake worth naming: the obvious reading of
/// "strip the root motion" takes all three.
///
/// Done *after* the animation is evaluated rather than by editing the clips. Patching assets would
/// have to be redone on every re-import and would be silently undone by a fresh
/// `tools/setup-assets`; this cannot drift, and it is also the only version that survives a blend,
/// where the pose is a mixture of two travelling curves and neither is the one to correct.
///
/// About 5 cm of genuine sideways sway goes with it, since a hip that sways and a hip that travels
/// are the same track. That is the price, it is below noticing at this size, and separating them
/// would mean subtracting a straight-line fit per clip — which a blend of two clips defeats again.
fn stay_where_the_simulation_put_them(mut hips: Query<(&Planted, &mut Transform)>) {
    for (rest, mut pose) in hips.iter_mut() {
        pose.translation.x = rest.0.x;
        pose.translation.z = rest.0.y;
    }
}

/// Marks a body whose skeleton has been wired, so it is done once rather than every frame.
#[derive(Component)]
struct Wired;

/// Update: gives a body's skeleton the animation plumbing its own file did not come with.
///
/// **This is the part that is not obvious.** Bevy's glTF loader builds its list of animation roots
/// while walking a file's *animations* — so a file with none, which is exactly what a character
/// model without clips is, comes out with a full skeleton and no `AnimationPlayer` on it and no
/// `AnimationTargetId` on any bone. Nothing is missing from the file and nothing is wrong with the
/// loader; the plumbing simply had nothing to be built from. Left alone the figure appears,
/// correctly posed, and never moves — which is precisely what it did.
///
/// So it is laid by hand, the same way the loader lays it: one bone is the animation root, and
/// every bone under it is labelled with the hash of the chain of names from that root down —
/// `Armature/root/pelvis/spine_01` and so on. The clips were labelled from the same paths when
/// *they* were loaded, which is what makes them meet.
///
/// **Which bone is the root is found rather than assumed**, and that is not defensive coding. The
/// first version took the spawned scene's top entity, which was wrong: Bevy wraps a spawned scene
/// in a node of its own, so every path came out as `Scene/Armature/root/...` against a library
/// spelling `Armature/root/...`, and not one bone of seventy-three was recognised. A wrapper is
/// Bevy's business and may change; what cannot change is that the right root is the one whose
/// paths the library knows. So every candidate is tried and the best-matching one wins, which is
/// both the fix and a check that the two files agree at all.
fn wire_up_the_skeleton(
    unwired: Query<(Entity, &ChildOf, Option<&Children>), NotWiredYet>,
    children: Query<&Children>,
    named: Query<&Name>,
    already: Query<(), With<AnimationPlayer>>,
    poses: Query<&Transform>,
    assets: Res<CharacterAssets>,
    clips: Res<Moves>,
    library: Res<Assets<AnimationClip>>,
    mut commands: Commands,
) {
    for (body, hangs_from, spawned) in unwired.iter() {
        // No children yet: the scene has not spawned. Ordinary for a body's first frames.
        if spawned.is_none_or(Children::is_empty) {
            continue;
        }
        let Some(clip) = library.get(&clips.probe) else {
            continue;
        };
        let Some((root, labelled, known)) =
            find_the_animation_root(body, &children, &named, clip)
        else {
            error!(
                "nothing under {body} answers to a bone path this library knows. The model and \
                 the clips are on different skeletons — `tools/glb rigs` compares the two files.",
            );
            commands.entity(body).insert(Wired);
            continue;
        };

        commands.entity(root).insert((
            Animates(hangs_from.parent()),
            AnimationGraphHandle(clips.graph.clone()),
            AnimationTransitions::new(),
        ));
        // A model that brought its own clips already has all of this, and doing it twice would
        // relabel bones the loader had labelled correctly.
        if !already.contains(root) {
            commands.entity(root).insert(AnimationPlayer::default());
            let name = named.get(root).cloned().unwrap_or_default();
            label_the_bones(root, name, &children, &named, &mut commands);
        }
        plant_the_hips(root, assets.kit, &children, &named, &poses, &mut commands);
        info!("{known} of {labelled} bones under {root} are animated by the library");
        commands.entity(body).insert(Wired);
    }
}

/// Notes where the root-motion bone rests, so the travel can be taken back out every frame.
///
/// Read here rather than in `PostUpdate` because here it is still the bind pose: the scene has
/// spawned, the `AnimationPlayer` is being added in this same run, and nothing has evaluated a
/// clip over it yet. One frame later the number would be whatever the animation had just written,
/// which is the value this exists to undo.
fn plant_the_hips(
    root: Entity,
    kit: &Kit,
    children: &Query<&Children>,
    named: &Query<&Name>,
    poses: &Query<&Transform>,
    commands: &mut Commands,
) {
    let Some(wanted) = kit.root_motion else {
        return;
    };
    let found = children
        .iter_descendants(root)
        .find(|bone| named.get(*bone).is_ok_and(|name| name.as_str() == wanted));
    let Some(bone) = found else {
        error!("{wanted} is not a bone under {root}, so its clips' own travel cannot be undone");
        return;
    };
    let Ok(pose) = poses.get(bone) else {
        return;
    };
    commands
        .entity(bone)
        .insert(Planted(Vec2::new(pose.translation.x, pose.translation.z)));
}

/// Which bone under `body` the library's paths are spelt from, and how well it fits.
///
/// Every named descendant is a candidate, because the depth the scene's own root sits at is
/// Bevy's business rather than a fact about the model. The winner is whichever candidate the clip
/// recognises the most bones under — a wrong root recognises none at all rather than a few, so
/// this is a choice between one answer and nothing, not a close-run thing.
fn find_the_animation_root(
    body: Entity,
    children: &Query<&Children>,
    named: &Query<&Name>,
    clip: &AnimationClip,
) -> Option<(Entity, usize, usize)> {
    let mut best: Option<(Entity, usize, usize)> = None;
    for candidate in children.iter_descendants(body) {
        let Ok(name) = named.get(candidate) else {
            continue;
        };
        let (labelled, known) = count_what_the_clip_knows(candidate, name.clone(), children, named, clip);
        if known > 0 && best.is_none_or(|(_, _, most)| known > most) {
            best = Some((candidate, labelled, known));
        }
    }
    best
}

/// How many bones under `root` the clip has a curve for, if the paths are spelt from `root` down.
fn count_what_the_clip_knows(
    root: Entity,
    root_name: Name,
    children: &Query<&Children>,
    named: &Query<&Name>,
    clip: &AnimationClip,
) -> (usize, usize) {
    let mut labelled = 0;
    let mut known = 0;
    walk_the_bones(root, root_name, children, named, &mut |_, id| {
        labelled += 1;
        known += usize::from(clip.curves_for_target(id).is_some());
    });
    (labelled, known)
}

/// Labels every bone under `root` with the hash of its path of names.
fn label_the_bones(
    root: Entity,
    root_name: Name,
    children: &Query<&Children>,
    named: &Query<&Name>,
    commands: &mut Commands,
) {
    walk_the_bones(root, root_name, children, named, &mut |bone, id| {
        commands.entity(bone).insert((id, AnimatedBy(root)));
    });
}

/// Walks `root` and everything named under it, handing each one the id its path hashes to.
fn walk_the_bones(
    root: Entity,
    root_name: Name,
    children: &Query<&Children>,
    named: &Query<&Name>,
    each: &mut impl FnMut(Entity, AnimationTargetId),
) {
    let mut walking = vec![(root, vec![root_name])];
    while let Some((bone, path)) = walking.pop() {
        each(bone, AnimationTargetId::from_names(path.iter()));
        let Ok(below) = children.get(bone) else {
            continue;
        };
        for child in below.iter() {
            let Ok(name) = named.get(child) else {
                continue;
            };
            let mut deeper = path.clone();
            deeper.push(name.clone());
            walking.push((child, deeper));
        }
    }
}

/// Update: plays the clip that matches what the simulation says the player is doing.
///
/// Read from `PlayerState` and `Aim` rather than from watching the transform move. The simulation
/// already knows whether a player is on the ground and whether they are crouching, and
/// differencing positions would turn interpolation's smoothing into a flicker in the choice of
/// clip.
fn choose_the_motion(
    moves: Res<Moves>,
    assets: Res<CharacterAssets>,
    bodies: Query<(&PlayerState, &Aim)>,
    mut skeletons: Query<(&Animates, &mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    for (owner, mut player, mut transitions) in skeletons.iter_mut() {
        let Ok((state, aim)) = bodies.get(owner.0) else {
            continue;
        };
        let (wanted, rate) = what_they_are_doing(assets.kit, state, aim.yaw);
        let Some(node) = moves.node(wanted) else {
            continue;
        };
        if transitions.get_main_animation() != Some(node) {
            transitions.play(&mut player, node, BLEND).repeat();
        }
        if let Some(playing) = player.animation_mut(node) {
            playing.set_speed(rate);
        }
    }
}

/// Which clip a player's state calls for, and how fast to play it.
///
/// Horizontal speed only: falling is not walking, and a player dropping off a ledge at 12 m/s
/// should not have their legs sprint. Being off the ground wins over everything else, because a
/// crouch clip played in mid-air is a person sitting in the sky.
fn what_they_are_doing(kit: &Kit, state: &PlayerState, yaw: f32) -> (Move, f32) {
    let travel = state.velocity.with_y(0.0);
    let speed = travel.length();
    let gait = if !state.on_ground {
        Gait::Airborne
    } else if state.crouching {
        if speed < STILL { Gait::CrouchIdle } else { Gait::CrouchWalk }
    } else if speed < STILL {
        Gait::Idle
    } else if speed < kit.trot() {
        Gait::Walk
    } else {
        Gait::Run
    };
    // A standing player has no direction of travel to read, and asking for one would hand back
    // whatever numerical noise is left in a velocity that has decayed to nothing.
    let facing = if speed < STILL { Facing::Forward } else { Facing::of(travel, yaw) };
    let rate = match kit.pace(gait) {
        Some(authored) => (speed / authored).clamp(RATE.0, RATE.1),
        None => 1.0,
    };
    ((gait, facing), rate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::movement::{CROUCH_SPEED, MAX_SPEED};

    fn walking(velocity: Vec3) -> PlayerState {
        PlayerState { velocity, on_ground: true, ..PlayerState::default() }
    }

    /// The drawn figure has to be no taller than the collision capsule, because that capsule is the
    /// hitbox and nothing else is. A body that reached above it would give every player a head they
    /// can see, aim at, and never hit.
    #[test]
    fn neither_kit_is_drawn_taller_than_the_hitbox() {
        for kit in [&SWAT, &MANNEQUIN] {
            let drawn = kit.height * kit.scale();
            assert!(
                drawn <= CAPSULE_HEIGHT + 1e-4,
                "{} is drawn {drawn} m tall inside a {CAPSULE_HEIGHT} m hitbox",
                kit.body,
            );
            assert!(
                drawn > CAPSULE_HEIGHT - 0.05,
                "{} is drawn {drawn} m tall in a {CAPSULE_HEIGHT} m hitbox, which leaves a gap",
                kit.body,
            );
        }
    }

    /// Walking straight forward has to read as forward, and the seven others have to land on their
    /// own clip. Sweeping the whole circle is worth more than eight chosen cases: it is the
    /// *boundaries* that are interesting, and they move when the rounding does.
    #[test]
    fn every_direction_of_travel_lands_on_its_own_clip() {
        // Dead ahead is −Z, and each eighth-turn to the left of it.
        let expected: [(f32, Facing); 8] = [
            (0.0, Facing::Forward),
            (45.0, Facing::ForwardLeft),
            (90.0, Facing::Left),
            (135.0, Facing::BackwardLeft),
            (180.0, Facing::Backward),
            (225.0, Facing::BackwardRight),
            (270.0, Facing::Right),
            (315.0, Facing::ForwardRight),
        ];
        for (degrees, wanted) in expected {
            let travel = Quat::from_rotation_y(degrees.to_radians()) * Vec3::NEG_Z;
            assert_eq!(Facing::of(travel, 0.0), wanted, "at {degrees} degrees");
        }
    }

    /// And it has to mean the same thing however the player is turned, because a body strafes
    /// relative to its own head rather than relative to the world. This is the half that a test
    /// against a player facing north cannot see at all.
    #[test]
    fn direction_is_read_in_the_players_own_frame() {
        for yaw in [0.0, 0.7, 1.9, -2.4, 3.1] {
            let right = Quat::from_rotation_y(yaw) * Vec3::X;
            assert_eq!(Facing::of(right, yaw), Facing::Right, "yawed by {yaw}");
            let ahead = Quat::from_rotation_y(yaw) * Vec3::NEG_Z;
            assert_eq!(Facing::of(ahead, yaw), Facing::Forward, "yawed by {yaw}");
        }
    }

    /// A direction that is nearly one of the eight has to round to it rather than to a neighbour,
    /// which is what the half-step before rounding is for.
    #[test]
    fn a_direction_rounds_to_the_nearest_of_the_eight() {
        for offset in [-22.0f32, -10.0, 0.0, 10.0, 22.0] {
            let travel = Quat::from_rotation_y(offset.to_radians()) * Vec3::NEG_Z;
            assert_eq!(Facing::of(travel, 0.0), Facing::Forward, "{offset} degrees off forward");
        }
    }

    /// Every speed the simulation can produce has to land on a clip that exists, at a rate that is
    /// not absurd — for both kits, because the fallback's clips are paced quite differently.
    #[test]
    fn every_speed_a_player_can_reach_picks_a_clip() {
        for kit in [&SWAT, &MANNEQUIN] {
            for step in 0..=110 {
                let speed = step as f32 * MAX_SPEED / 100.0;
                for crouching in [false, true] {
                    let state = PlayerState {
                        velocity: Vec3::new(speed, -3.0, 0.0),
                        on_ground: true,
                        crouching,
                        ..PlayerState::default()
                    };
                    let (wanted, rate) = what_they_are_doing(kit, &state, 0.0);
                    assert!(
                        (RATE.0..=RATE.1).contains(&rate),
                        "{:?} at {speed} m/s wants to play at {rate}x",
                        wanted,
                    );
                    assert!(
                        every_move().contains(&wanted),
                        "{wanted:?} is not a movement any kit was asked to load",
                    );
                }
            }
        }
    }

    /// Falling is not walking. A player who steps off a ledge keeps whatever horizontal speed they
    /// had, and reading that as locomotion would run their legs in mid-air.
    #[test]
    fn a_player_in_the_air_is_not_running() {
        let state = PlayerState {
            velocity: Vec3::new(MAX_SPEED, -12.0, 0.0),
            on_ground: false,
            ..PlayerState::default()
        };
        assert_eq!(what_they_are_doing(&SWAT, &state, 0.0).0.0, Gait::Airborne);
    }

    /// Vertical speed must not reach the choice at all — the clearest way to say it is that a
    /// player standing still on the ground is idle however fast the solver thinks they are sinking.
    #[test]
    fn falling_speed_is_not_walking_speed() {
        let state = walking(Vec3::new(0.0, -20.0, 0.0));
        assert_eq!(what_they_are_doing(&SWAT, &state, 0.0).0.0, Gait::Idle);
    }

    /// The two locomotion cycles have to be stretched by as little as possible, and the crossover
    /// is what decides that. At it both are stretched by the same ratio — which is what makes it
    /// the geometric mean rather than the arithmetic one, and is the thing that silently breaks if
    /// somebody replaces it with a round number.
    #[test]
    fn the_crossover_stretches_both_cycles_alike() {
        for kit in [&SWAT, &MANNEQUIN] {
            let walk = kit.trot() / kit.paces.walk;
            let run = kit.paces.run / kit.trot();
            assert!(
                (walk - run).abs() < 0.02,
                "{}: at {} m/s the walk is stretched {walk}x and the run {run}x",
                kit.body,
                kit.trot(),
            );
        }
    }

    /// The soldier's clips are paced for this game and the fallback's are not, and that difference
    /// is the whole reason both kits exist. Stated as a test so that replacing either pack says
    /// which side of the line it lands on.
    #[test]
    fn the_soldier_never_has_to_be_stretched_and_the_fallback_does() {
        let running = walking(Vec3::new(MAX_SPEED, 0.0, 0.0));
        let crouching = PlayerState {
            velocity: Vec3::new(CROUCH_SPEED, 0.0, 0.0),
            crouching: true,
            ..walking(Vec3::ZERO)
        };
        for state in [running, crouching] {
            let (_, soldier) = what_they_are_doing(&SWAT, &state, 0.0);
            assert!(
                (0.8..=1.4).contains(&soldier),
                "the soldier is being stretched {soldier}x, which it never used to be",
            );
        }
        let (_, fallback) = what_they_are_doing(&MANNEQUIN, &crouching, 0.0);
        assert_eq!(fallback, RATE.1, "the fallback crouch no longer runs into the clamp");
    }

    /// Each kit has to have a name for every movement the game can ask for, or a player freezes
    /// mid-stride on whichever one is missing.
    #[test]
    fn both_kits_name_every_movement() {
        for kit in [&SWAT, &MANNEQUIN] {
            for wanted in every_move() {
                assert!(kit.clip(wanted).is_some(), "{} has no clip for {wanted:?}", kit.body);
            }
        }
    }

    /// Exactly one kit carries root motion, and which one is a measurement rather than a
    /// preference — the committed fallback pack is the variant that was exported with it stripped,
    /// and the soldier's is not. Stated here so that replacing either pack says so out loud
    /// instead of showing up as a figure sliding away from its own hitbox.
    #[test]
    fn only_the_soldiers_clips_carry_their_own_travel() {
        assert_eq!(SWAT.root_motion, Some("mixamorig:Hips"));
        assert_eq!(MANNEQUIN.root_motion, None);
    }

    /// The soldier's eight directions have to be eight *different* clips, or the pack is not what
    /// it was chosen for. The fallback's are deliberately all one, which is the same assertion
    /// read the other way.
    #[test]
    fn only_one_of_the_kits_can_actually_strafe() {
        let names = |kit: &Kit, gait| {
            let mut seen: Vec<String> =
                Facing::ALL.iter().filter_map(|f| kit.clip((gait, *f))).collect();
            seen.sort();
            seen.dedup();
            seen.len()
        };
        assert_eq!(names(&SWAT, Gait::Run), 8, "the soldier should have eight run clips");
        assert_eq!(names(&MANNEQUIN, Gait::Run), 1, "the fallback should have exactly one");
    }
}
