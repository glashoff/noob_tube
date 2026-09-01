//! Drawing a player as a person rather than as a capsule.
//!
//! A player entity arrives from the server carrying [`PlayerState`] and [`Aim`] and nothing
//! visible. This hangs a character model under it and picks the animation that matches what the
//! simulation says the player is doing — standing, walking, running, crouching or in the air.
//!
//! **The model and the clips come from different files**, and that only works because they share a
//! skeleton exactly. Bevy binds an animation to a bone by the *path* of names from the scene root
//! down, so `Armature/root/pelvis/spine_01` has to be spelt the same in both. It is:
//!
//! ```text
//! tools/glb rigs assets/animations/*.glb assets/characters/*.gltf
//! ```
//!
//! reports the five files in `assets/` as identical. That check is the whole reason this is a
//! handful of systems rather than a retargeting project — see the README under "Character assets".

use bevy::animation::{AnimatedBy, AnimationTargetId};
use bevy::gltf::Gltf;
use bevy::prelude::*;
use core::time::Duration;
use lightyear::prelude::*;
use noob_tube_shared::movement::CAPSULE_HEIGHT;
use noob_tube_shared::player::{Player, PlayerState};

/// The body, under the asset directory.
///
/// One of two that ship — `Superhero_Female_FullBody.gltf` is the other, and swapping this line is
/// the whole of using it. Both are CC0; see `assets/CREDITS.md`.
const BODY: &str = "characters/Superhero_Male_FullBody.gltf";
/// The clip library, under the asset directory. 43 animations on the same skeleton.
const CLIPS: &str = "animations/universal_animation_library_1.glb";

/// How tall the body is in its own file, standing, in metres.
///
/// Measured rather than guessed — `tools/glb` reads it back out of the mesh's own bounds:
///
/// ```text
/// tools/glb nodes assets/characters/Superhero_Male_FullBody.gltf
/// ```
///
/// The female body is 1.767 by the same measure, so replacing [`BODY`] means replacing this too.
const MODEL_HEIGHT: f32 = 1.810;

/// What the model is scaled by so that it is exactly as tall as the collision capsule.
///
/// Not decoration. The capsule *is* the hitbox — a shot is tested against it and nothing else — so
/// a model drawn at its own 1.81 m inside a 1.70 m capsule would put 11 cm of head where a player
/// can aim and no raycast can reach. That mistake has been made here once already, with the
/// placeholder head box this replaces, and it is invisible until somebody complains that head
/// shots do not register.
///
/// What this does *not* fix is width: a character with an arm out reaches past a 35 cm capsule and
/// no uniform scale changes that. It is the honest cost of a real model over a capsule, and the
/// argument for the per-bone hitboxes under "Still to settle" in the README.
const MODEL_SCALE: f32 = CAPSULE_HEIGHT / MODEL_HEIGHT;

/// Half a turn, because glTF says a model faces +Z and this game says forward is −Z.
///
/// Confirmed against the file rather than taken from the specification: the eyes and eyebrows sit
/// at z 0.04 to 0.09, on the +Z side of the head.
const MODEL_YAW: f32 = core::f32::consts::PI;

/// How long a change of animation takes to blend.
///
/// Long enough that starting to walk is not a snap, short enough that it is not a slide. A tenth of
/// a second is about one frame of a run cycle at these speeds.
const BLEND: Duration = Duration::from_millis(120);

/// The clips this game asks for, by the names they carry in the library.
///
/// Fewer than the library holds, and deliberately: a name that is not here is one nothing in the
/// simulation can ask for. `Sprint_Loop` is absent because there is no sprint input — the ground
/// speed tops out at 5.5 m/s and the jog cycle is paced for 5.36.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Motion {
    Idle,
    Walk,
    Jog,
    CrouchIdle,
    CrouchWalk,
    Airborne,
}

impl Motion {
    /// The clip's name in the library.
    fn clip(self) -> &'static str {
        match self {
            Motion::Idle => "Idle_Loop",
            Motion::Walk => "Walk_Loop",
            Motion::Jog => "Jog_Fwd_Loop",
            Motion::CrouchIdle => "Crouch_Idle_Loop",
            Motion::CrouchWalk => "Crouch_Fwd_Loop",
            Motion::Airborne => "Jump_Loop",
        }
    }

    /// The ground speed the clip's stride was authored for, in metres per second.
    ///
    /// This is what foot lock needs: play a walk cycle at a speed its stride was not made for and
    /// the feet skate along the ground. Playing it at `actual / natural` puts the stride back in
    /// step with the travel.
    ///
    /// The numbers are measured, not guessed, and they cannot be measured from the file in
    /// `assets/` — root motion is stripped there, which is why it is the variant that is in. They
    /// come from the `_RM` variant of the same download, where the root bone still travels:
    ///
    /// ```text
    /// tools/glb animations 'Universal Animation Library[Standard]/Unreal-Godot/UAL1_Standard_RM.glb'
    /// ```
    ///
    /// prints `travels 1.30 m, 0.97 m/s` for the walk and `5.00 m, 5.36 m/s` for the jog. A clip
    /// that does not travel returns `None` and is played at its own pace.
    fn natural_speed(self) -> Option<f32> {
        match self {
            Motion::Walk => Some(0.97),
            Motion::Jog => Some(5.36),
            Motion::CrouchWalk => Some(0.75),
            Motion::Idle | Motion::CrouchIdle | Motion::Airborne => None,
        }
    }

    fn all() -> [Motion; 6] {
        [
            Motion::Idle,
            Motion::Walk,
            Motion::Jog,
            Motion::CrouchIdle,
            Motion::CrouchWalk,
            Motion::Airborne,
        ]
    }
}

/// Below this, a player is standing still rather than walking slowly.
///
/// A shade under a tenth of a metre a second. Lower and a player who has stopped keeps shuffling on
/// the last of their deceleration; higher and the first step of a walk is a slide.
const STILL: f32 = 0.08;

/// Where the walk cycle gives way to the jog.
///
/// The geometric mean of the two clips' natural speeds, `sqrt(0.97 * 5.36)`, which is the crossover
/// that keeps the *ratio* each clip is stretched by as small as it can be. The two are far apart
/// and there is nothing between them — the free library has forward locomotion at a walk and at a
/// jog and nothing else — so between about 1.5 and 3.5 m/s one or the other is being pushed hard.
/// It matters less than it sounds: movement ramps to full speed in 0.3 s, so that band is passed
/// through rather than lived in.
const TROT: f32 = 2.28;

/// How far a clip may be sped up or slowed down before it is left alone to skate a little.
///
/// A stride played at three times its pace does not read as running fast, it reads as broken. The
/// crouch is what runs into this: it is authored for 0.75 m/s and this game crouch-walks at 2.6.
const RATE: (f32, f32) = (0.5, 2.0);

pub struct CharacterPlugin;

impl Plugin for CharacterPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_the_character)
            .add_systems(
                Update,
                (
                    build_the_clip_graph.run_if(not(resource_exists::<Clips>)),
                    give_bodies,
                    // Both need the graph, and it does not exist until the library has finished
                    // loading — a few frames in, and longer on a cold disk. Gating them is what
                    // keeps that from being a panic on the first frame.
                    (wire_up_the_skeleton, choose_the_motion).run_if(resource_exists::<Clips>),
                )
                    .chain()
                    // The same reason `remote_players` waits: interpolation writes the smoothed
                    // `PlayerState` in Update, and choosing a clip from last frame's sample would
                    // add a frame of lag to a decision that is already a frame behind.
                    .after(InterpolationSystems::All),
            );
    }
}

/// What is loaded, before anything is known about it.
#[derive(Resource)]
struct CharacterAssets {
    body: Handle<WorldAsset>,
    library: Handle<Gltf>,
}

/// The animation graph, once the library has finished loading, and where each clip sits in it.
#[derive(Resource)]
struct Clips {
    graph: Handle<AnimationGraph>,
    nodes: Vec<(Motion, AnimationNodeIndex)>,
    /// One clip, kept only so the wiring can check itself against it. See
    /// [`wire_up_the_skeleton`], which counts how many of the bones it just labelled this clip
    /// actually has a curve for — a number that is either "most of them" or "none".
    probe: Handle<AnimationClip>,
}

impl Clips {
    fn node(&self, motion: Motion) -> Option<AnimationNodeIndex> {
        self.nodes.iter().find(|(which, _)| *which == motion).map(|(_, node)| *node)
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

/// On the entity Bevy gave an [`AnimationPlayer`] to, naming the player it belongs to.
///
/// The link has to be stored because the skeleton arrives asynchronously and several levels down:
/// by the time there is an `AnimationPlayer` to drive, the player entity is four parents away.
#[derive(Component)]
struct Animates(Entity);

/// Startup: asks for the body and the clip library.
fn load_the_character(assets: Res<AssetServer>, mut commands: Commands) {
    commands.insert_resource(CharacterAssets {
        body: assets.load(GltfAssetLabel::Scene(0).from_asset(BODY)),
        library: assets.load::<Gltf>(CLIPS),
    });
}

/// Update, until it succeeds: builds the animation graph once the library has loaded.
///
/// By name rather than by index. The library holds 43 clips and glTF gives them no order worth
/// relying on; `Idle_Loop` will still be `Idle_Loop` after the pack is updated, where clip 9 will
/// not be. A name that is missing is reported once and then simply cannot be asked for.
fn build_the_clip_graph(
    assets: Res<CharacterAssets>,
    library: Res<Assets<Gltf>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut commands: Commands,
) {
    let Some(gltf) = library.get(&assets.library) else {
        return;
    };
    let mut graph = AnimationGraph::new();
    let mut nodes = Vec::new();
    let mut probe = None;
    for motion in Motion::all() {
        let Some(clip) = gltf.named_animations.get(motion.clip()) else {
            error!("{CLIPS} has no clip called {}", motion.clip());
            continue;
        };
        probe.get_or_insert_with(|| clip.clone());
        nodes.push((motion, graph.add_clip(clip.clone(), 1.0, graph.root)));
    }
    let Some(probe) = probe else {
        error!("{CLIPS} held none of the clips this game asks for");
        return;
    };
    info!("{} animation clips ready", nodes.len());
    commands.insert_resource(Clips { graph: graphs.add(graph), nodes, probe });
}

/// Update: hangs a model under a player that has just arrived.
///
/// `Without<Predicted>` leaves our own player out: the camera sits inside it, and a body there
/// would fill the screen. It is a child rather than the player entity itself because the two want
/// different transforms — the player entity carries the pose the simulation writes, in world units
/// and facing −Z, and this carries the half turn and the scale that make a glTF model agree with
/// that.
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
                    .with_scale(Vec3::splat(MODEL_SCALE)),
            ));
        info!("drawing player {}", player.peer);
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
    clips: Res<Clips>,
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
        info!("{known} of {labelled} bones under {root} are animated by the library");
        commands.entity(body).insert(Wired);
    }
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
/// Read from `PlayerState` rather than from the frame-to-frame movement of the transform. The state
/// is the simulation's own answer — it already knows whether a player is on the ground and whether
/// they are crouching — and differencing positions would turn interpolation's smoothing into a
/// jitter in the choice of clip.
fn choose_the_motion(
    clips: Res<Clips>,
    bodies: Query<&PlayerState>,
    mut skeletons: Query<(&Animates, &mut AnimationPlayer, &mut AnimationTransitions)>,
) {
    for (owner, mut player, mut transitions) in skeletons.iter_mut() {
        let Ok(state) = bodies.get(owner.0) else {
            continue;
        };
        let (motion, rate) = what_they_are_doing(state);
        let Some(node) = clips.node(motion) else {
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
fn what_they_are_doing(state: &PlayerState) -> (Motion, f32) {
    let speed = state.velocity.with_y(0.0).length();
    let motion = if !state.on_ground {
        Motion::Airborne
    } else if state.crouching {
        if speed < STILL { Motion::CrouchIdle } else { Motion::CrouchWalk }
    } else if speed < STILL {
        Motion::Idle
    } else if speed < TROT {
        Motion::Walk
    } else {
        Motion::Jog
    };
    let rate = match motion.natural_speed() {
        Some(natural) => (speed / natural).clamp(RATE.0, RATE.1),
        None => 1.0,
    };
    (motion, rate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::movement::{CROUCH_SPEED, MAX_SPEED};

    /// The drawn figure has to be no taller than the collision capsule, because that capsule is the
    /// hitbox and nothing else is. A model that reached above it would give every player a head
    /// they can see, aim at, and never hit.
    #[test]
    fn the_model_is_drawn_no_taller_than_the_hitbox() {
        let drawn = MODEL_HEIGHT * MODEL_SCALE;
        assert!(
            drawn <= CAPSULE_HEIGHT + 1e-4,
            "the model is drawn {drawn} m tall inside a {CAPSULE_HEIGHT} m hitbox",
        );
        // And not so much shorter that the head is buried inside the capsule with room above it.
        assert!(
            drawn > CAPSULE_HEIGHT - 0.05,
            "the model is drawn {drawn} m tall in a {CAPSULE_HEIGHT} m hitbox, which leaves a gap",
        );
    }

    /// Every speed the simulation can produce has to land on a clip, or a player stands frozen in
    /// the middle of a sprint. Sweeping is worth more than three chosen cases: the interesting
    /// failures are at the boundaries, and the boundaries move when the constants do.
    #[test]
    fn every_speed_a_player_can_reach_picks_a_clip() {
        for step in 0..=110 {
            let speed = step as f32 * MAX_SPEED / 100.0;
            for crouching in [false, true] {
                let state = PlayerState {
                    velocity: Vec3::new(speed, -3.0, 0.0),
                    on_ground: true,
                    crouching,
                    ..PlayerState::default()
                };
                let (motion, rate) = what_they_are_doing(&state);
                assert!(
                    (RATE.0..=RATE.1).contains(&rate),
                    "{motion:?} at {speed} m/s wants to play at {rate}x",
                );
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
        assert_eq!(what_they_are_doing(&state).0, Motion::Airborne);
    }

    /// Vertical speed must not reach the choice at all — the clearest way to say it is that a
    /// player standing still on the ground is idle however fast the solver thinks they are sinking.
    #[test]
    fn falling_speed_is_not_walking_speed() {
        let state = PlayerState {
            velocity: Vec3::new(0.0, -20.0, 0.0),
            on_ground: true,
            ..PlayerState::default()
        };
        assert_eq!(what_they_are_doing(&state).0, Motion::Idle);
    }

    /// The two locomotion clips have to be stretched by as little as possible, and the crossover is
    /// what decides that. At the crossover both are stretched by the same ratio — which is what
    /// makes `TROT` the geometric mean rather than the arithmetic one, and is the thing that
    /// silently breaks if somebody rounds it.
    #[test]
    fn the_crossover_stretches_both_clips_alike() {
        let walk = TROT / Motion::Walk.natural_speed().expect("the walk travels");
        let jog = Motion::Jog.natural_speed().expect("the jog travels") / TROT;
        assert!(
            (walk - jog).abs() < 0.02,
            "at {TROT} m/s the walk is stretched {walk}x and the jog {jog}x",
        );
    }

    /// A crouch-walk is the one place the library cannot keep up, and that is worth stating rather
    /// than discovering: the clip paces 0.75 m/s and this game crouches at 2.6, so it is played at
    /// the clamp and the feet do skate. If the crouch speed or the clip ever changes, this is the
    /// test that says the compromise is gone.
    #[test]
    fn the_crouch_walk_is_the_clip_that_cannot_keep_up() {
        let state = PlayerState {
            velocity: Vec3::new(CROUCH_SPEED, 0.0, 0.0),
            on_ground: true,
            crouching: true,
            ..PlayerState::default()
        };
        let (motion, rate) = what_they_are_doing(&state);
        assert_eq!(motion, Motion::CrouchWalk);
        assert_eq!(rate, RATE.1, "the crouch no longer runs into the clamp");
    }

    /// Not a real test of behaviour, but of an assumption everything above rests on: the clip names
    /// are the ones the library actually carries. Spelling is checked against the file by
    /// `tools/glb animations`, and this at least holds the list to one place.
    #[test]
    fn every_motion_names_a_distinct_clip() {
        let mut seen: Vec<&str> = Motion::all().iter().map(|motion| motion.clip()).collect();
        seen.sort_unstable();
        let before = seen.len();
        seen.dedup();
        assert_eq!(before, seen.len(), "two motions ask for the same clip");
    }

}
