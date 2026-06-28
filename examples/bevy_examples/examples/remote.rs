use bevy::prelude::*;
use bevy::remote::{RemotePlugin, http::RemoteHttpPlugin};
use bevy_motiongfx::BevyMotionGfxPlugin;
use bevy_motiongfx::prelude::*;
use bevy_motiongfx::remote::{
    MotionGfxAnimatableApp, MotionGfxProjectApp, MotionGfxRemotePlugin,
    timeline_create,
};

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins,
            BevyMotionGfxPlugin,
            MotionGfxRemotePlugin::extend(RemotePlugin::default()),
            MotionGfxRemotePlugin,
            RemoteHttpPlugin::default().with_port(3030),
        ))
        .register_animatable::<Transform, f32, _>(path!(
            <Transform>::translation::x
        ))
        .register_animatable::<Transform, f32, _>(path!(
            <Transform>::translation::y
        ))
        .register_animatable::<Transform, f32, _>(path!(
            <Transform>::translation::z
        ))
        .register_animatable::<Transform, f32, _>(path!(
            <Transform>::scale::x
        ))
        .register_animatable::<Transform, f32, _>(path!(
            <Transform>::scale::y
        ))
        .register_animatable::<Sprite, Color, _>(path!(
            <Sprite>::color
        ))
        .register_animatable_resource::<ClearColor, Color, _>(
            path!(<ClearColor>::0),
        )
        .register_project_subject::<Sprite>()
        .add_systems(Startup, |world: &mut World| {
            let _ = timeline_create(In(None), world);
        })
        .run();
}
