"""生成端到端测试场景：每帧渲染耗时约 2 秒，便于中途强杀验证续接。"""
import bpy
import os

OUT = os.path.dirname(os.path.abspath(__file__))
FRAMES = os.path.join(OUT, "frames")
os.makedirs(FRAMES, exist_ok=True)

sc = bpy.context.scene
for o in list(bpy.data.objects):
    bpy.data.objects.remove(o, do_unlink=True)

bpy.ops.mesh.primitive_uv_sphere_add(radius=1.0, segments=64, ring_count=32)
sphere = bpy.context.object
bpy.ops.object.shade_smooth()
sub = sphere.modifiers.new("Sub", 'SUBSURF')
sub.levels = 3
sub.render_levels = 3
sphere.location = (0, 0, 0)
sphere.keyframe_insert("rotation_euler", frame=1)
sphere.rotation_euler[2] = 1.2
sphere.keyframe_insert("rotation_euler", frame=8)

bpy.ops.object.camera_add(location=(0, -4.2, 0), rotation=(1.5708, 0, 0))
sc.camera = bpy.context.object
bpy.context.object.name = "MainCam"

bpy.ops.object.camera_add(location=(3.5, -2.5, 1.0), rotation=(1.35, 0, 0.94))
bpy.context.object.name = "AltCam"

bpy.ops.object.light_add(type='AREA', location=(3, -3, 4))
light = bpy.context.object
light.data.energy = 900
light.data.size = 3

world = bpy.data.worlds.new("W")
sc.world = world
world.use_nodes = True
world.node_tree.nodes["Background"].inputs[1].default_value = 0.35

sc.render.engine = 'CYCLES'
sc.cycles.device = 'CPU'
sc.cycles.samples = 4000
sc.cycles.use_denoising = False
sc.render.resolution_x = 480
sc.render.resolution_y = 480
sc.render.resolution_percentage = 100
sc.render.fps = 24
sc.render.image_settings.file_format = 'PNG'
sc.render.image_settings.color_mode = 'RGB'
sc.render.filepath = os.path.join(FRAMES, "frame_")
sc.frame_start = 1
sc.frame_end = 8
sc.frame_step = 1

bpy.ops.wm.save_as_mainfile(filepath=os.path.join(OUT, "scene.blend"))
print("TEST_SCENE_OK", sc.render.filepath)
