use super::tool_prelude::*;
use crate::consts::{LINE_ROTATE_SNAP_ANGLE, MANIPULATOR_GROUP_MARKER_SIZE, SELECTION_THRESHOLD};
use crate::messages::portfolio::document::overlays::utility_types::OverlayContext;
use crate::messages::portfolio::document::utility_types::document_metadata::LayerNodeIdentifier;
use crate::messages::tool::common_functionality::auto_panning::AutoPanning;
use crate::messages::tool::common_functionality::graph_modification_utils::{NodeGraphLayer, get_gradient};
use crate::messages::tool::common_functionality::snapping::SnapManager;
use graphene_core::vector::style::{Fill, Gradient, GradientType};

#[derive(Default)]
pub struct GradientTool {
	fsm_state: GradientToolFsmState,
	data: GradientToolData,
	options: GradientOptions,
}

#[derive(Default)]
pub struct GradientOptions {
	gradient_type: GradientType,
}

#[impl_message(Message, ToolMessage, Gradient)]
#[derive(PartialEq, Clone, Debug, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum GradientToolMessage {
	// Standard messages
	Abort,
	Overlays(OverlayContext),

	// Tool-specific messages
	DeleteStop,
	InsertStop,
	PointerDown,
	PointerMove { constrain_axis: Key },
	PointerOutsideViewport { constrain_axis: Key },
	PointerUp,
	UpdateOptions(GradientOptionsUpdate),
}

#[derive(PartialEq, Eq, Clone, Debug, Hash, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum GradientOptionsUpdate {
	Type(GradientType),
}

impl ToolMetadata for GradientTool {
	fn icon_name(&self) -> String {
		"GeneralGradientTool".into()
	}
	fn tooltip(&self) -> String {
		"Gradient Tool".into()
	}
	fn tool_type(&self) -> crate::messages::tool::utility_types::ToolType {
		ToolType::Gradient
	}
}

impl<'a> MessageHandler<ToolMessage, &mut ToolActionHandlerData<'a>> for GradientTool {
	fn process_message(&mut self, message: ToolMessage, responses: &mut VecDeque<Message>, tool_data: &mut ToolActionHandlerData<'a>) {
		let ToolMessage::Gradient(GradientToolMessage::UpdateOptions(action)) = message else {
			self.fsm_state.process_event(message, &mut self.data, tool_data, &self.options, responses, false);
			return;
		};
		match action {
			GradientOptionsUpdate::Type(gradient_type) => {
				self.options.gradient_type = gradient_type;
				// Update the selected gradient if it exists
				if let Some(selected_gradient) = &mut self.data.selected_gradient {
					// Check if the current layer is a raster layer
					if let Some(layer) = selected_gradient.layer {
						if NodeGraphLayer::is_raster_layer(layer, &mut tool_data.document.network_interface) {
							return; // Don't proceed if it's a raster layer
						}
						selected_gradient.gradient.gradient_type = gradient_type;
						selected_gradient.render_gradient(responses);
					}
				}
			}
		}
	}

	advertise_actions!(GradientToolMessageDiscriminant;
		PointerDown,
		PointerUp,
		PointerMove,
		Abort,
		InsertStop,
		DeleteStop,
	);
}

impl LayoutHolder for GradientTool {
	fn layout(&self) -> Layout {
		let gradient_type = RadioInput::new(vec![
			RadioEntryData::new("linear")
				.label("Linear")
				.tooltip("Linear gradient")
				.on_update(move |_| GradientToolMessage::UpdateOptions(GradientOptionsUpdate::Type(GradientType::Linear)).into()),
			RadioEntryData::new("radial")
				.label("Radial")
				.tooltip("Radial gradient")
				.on_update(move |_| GradientToolMessage::UpdateOptions(GradientOptionsUpdate::Type(GradientType::Radial)).into()),
		])
		.selected_index(Some((self.selected_gradient().unwrap_or(self.options.gradient_type) == GradientType::Radial) as u32))
		.widget_holder();

		Layout::WidgetLayout(WidgetLayout::new(vec![LayoutGroup::Row { widgets: vec![gradient_type] }]))
	}
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum GradientToolFsmState {
	#[default]
	Ready,
	Drawing,
}

/// Computes the transform from gradient space to viewport space (where gradient space is 0..1)
fn gradient_space_transform(layer: LayerNodeIdentifier, document: &DocumentMessageHandler) -> DAffine2 {
	let bounds = document.metadata().nonzero_bounding_box(layer);
	let bound_transform = DAffine2::from_scale_angle_translation(bounds[1] - bounds[0], 0., bounds[0]);

	let multiplied = document.metadata().transform_to_viewport(layer);

	multiplied * bound_transform
}

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub enum GradientDragTarget {
	Start,
	#[default]
	End,
	Step(usize),
}

/// Contains information about the selected gradient handle
#[derive(Clone, Debug, Default)]
struct SelectedGradient {
	layer: Option<LayerNodeIdentifier>,
	transform: DAffine2,
	gradient: Gradient,
	dragging: GradientDragTarget,
}

impl SelectedGradient {
	pub fn new(gradient: Gradient, layer: LayerNodeIdentifier, document: &DocumentMessageHandler) -> Self {
		let transform = gradient_space_transform(layer, document);
		Self {
			layer: Some(layer),
			transform,
			gradient,
			dragging: GradientDragTarget::End,
		}
	}

	pub fn with_gradient_start(mut self, start: DVec2) -> Self {
		self.gradient.start = self.transform.inverse().transform_point2(start);
		self
	}

	pub fn update_gradient(&mut self, mut mouse: DVec2, responses: &mut VecDeque<Message>, snap_rotate: bool, gradient_type: GradientType) {
		self.gradient.gradient_type = gradient_type;

		if snap_rotate && matches!(self.dragging, GradientDragTarget::End | GradientDragTarget::Start) {
			let point = if self.dragging == GradientDragTarget::Start {
				self.transform.transform_point2(self.gradient.end)
			} else {
				self.transform.transform_point2(self.gradient.start)
			};

			let delta = point - mouse;

			let length = delta.length();
			let mut angle = -delta.angle_to(DVec2::X);

			let snap_resolution = LINE_ROTATE_SNAP_ANGLE.to_radians();
			angle = (angle / snap_resolution).round() * snap_resolution;

			let rotated = DVec2::new(length * angle.cos(), length * angle.sin());
			mouse = point - rotated;
		}

		let transformed_mouse = self.transform.inverse().transform_point2(mouse);

		match self.dragging {
			GradientDragTarget::Start => self.gradient.start = transformed_mouse,
			GradientDragTarget::End => self.gradient.end = transformed_mouse,
			GradientDragTarget::Step(s) => {
				let (start, end) = (self.transform.transform_point2(self.gradient.start), self.transform.transform_point2(self.gradient.end));

				// Calculate the new position by finding the closest point on the line
				let new_pos = ((end - start).angle_to(mouse - start)).cos() * start.distance(mouse) / start.distance(end);

				// Should not go off end but can swap
				let clamped = new_pos.clamp(0., 1.);
				self.gradient.stops.get_mut(s).unwrap().0 = clamped;
				let new_pos = self.gradient.stops[s];

				self.gradient.stops.sort();
				self.dragging = GradientDragTarget::Step(self.gradient.stops.iter().position(|x| *x == new_pos).unwrap());
			}
		}
		self.render_gradient(responses);
	}

	/// Update the layer fill to the current gradient
	pub fn render_gradient(&mut self, responses: &mut VecDeque<Message>) {
		self.gradient.transform = self.transform;
		if let Some(layer) = self.layer {
			responses.add(GraphOperationMessage::FillSet {
				layer,
				fill: Fill::Gradient(self.gradient.clone()),
			});
		}
	}
}

impl GradientTool {
	/// Get the gradient type of the selected gradient (if it exists)
	pub fn selected_gradient(&self) -> Option<GradientType> {
		self.data.selected_gradient.as_ref().map(|selected| selected.gradient.gradient_type)
	}
}

impl ToolTransition for GradientTool {
	fn event_to_message_map(&self) -> EventToMessageMap {
		EventToMessageMap {
			tool_abort: Some(GradientToolMessage::Abort.into()),
			overlay_provider: Some(|overlay_context| GradientToolMessage::Overlays(overlay_context).into()),
			..Default::default()
		}
	}
}

#[derive(Clone, Debug, Default)]
struct GradientToolData {
	selected_gradient: Option<SelectedGradient>,
	snap_manager: SnapManager,
	drag_start: DVec2,
	auto_panning: AutoPanning,
}

impl Fsm for GradientToolFsmState {
	type ToolData = GradientToolData;
	type ToolOptions = GradientOptions;

	fn transition(self, event: ToolMessage, tool_data: &mut Self::ToolData, tool_action_data: &mut ToolActionHandlerData, tool_options: &Self::ToolOptions, responses: &mut VecDeque<Message>) -> Self {
		let ToolActionHandlerData {
			document, global_tool_data, input, ..
		} = tool_action_data;

		let ToolMessage::Gradient(event) = event else { return self };
		match (self, event) {
			(_, GradientToolMessage::Overlays(mut overlay_context)) => {
				let selected = tool_data.selected_gradient.as_ref();

				for layer in document.network_interface.selected_nodes().selected_visible_layers(&document.network_interface) {
					let Some(gradient) = get_gradient(layer, &document.network_interface) else { continue };
					let transform = gradient_space_transform(layer, document);
					let dragging = selected
						.filter(|selected| selected.layer.is_some_and(|selected_layer| selected_layer == layer))
						.map(|selected| selected.dragging);

					let Gradient { start, end, stops, .. } = gradient;
					let (start, end) = (transform.transform_point2(start), transform.transform_point2(end));

					overlay_context.line(start, end, None, None);
					overlay_context.manipulator_handle(start, dragging == Some(GradientDragTarget::Start), None);
					overlay_context.manipulator_handle(end, dragging == Some(GradientDragTarget::End), None);

					for (index, (position, _)) in stops.into_iter().enumerate() {
						if position.abs() < f64::EPSILON * 1000. || (1. - position).abs() < f64::EPSILON * 1000. {
							continue;
						}

						overlay_context.manipulator_handle(start.lerp(end, position), dragging == Some(GradientDragTarget::Step(index)), None);
					}
				}

				self
			}
			(GradientToolFsmState::Ready, GradientToolMessage::DeleteStop) => {
				let Some(selected_gradient) = &mut tool_data.selected_gradient else {
					return self;
				};

				// Skip if invalid gradient
				if selected_gradient.gradient.stops.len() < 2 {
					return self;
				}

				responses.add(DocumentMessage::AddTransaction);

				// Remove the selected point
				match selected_gradient.dragging {
					GradientDragTarget::Start => {
						selected_gradient.gradient.stops.remove(0);
					}
					GradientDragTarget::End => {
						let _ = selected_gradient.gradient.stops.pop();
					}
					GradientDragTarget::Step(index) => {
						selected_gradient.gradient.stops.remove(index);
					}
				};

				// The gradient has only one point and so should become a fill
				if selected_gradient.gradient.stops.len() == 1 {
					if let Some(layer) = selected_gradient.layer {
						responses.add(GraphOperationMessage::FillSet {
							layer,
							fill: Fill::Solid(selected_gradient.gradient.stops[0].1),
						});
					}
					return self;
				}

				// Find the minimum and maximum positions
				let min_position = selected_gradient.gradient.stops.iter().map(|(pos, _)| *pos).reduce(f64::min).expect("No min");
				let max_position = selected_gradient.gradient.stops.iter().map(|(pos, _)| *pos).reduce(f64::max).expect("No max");

				// Recompute the start and end position of the gradient (in viewport transform)
				let transform = selected_gradient.transform;
				let (start, end) = (transform.transform_point2(selected_gradient.gradient.start), transform.transform_point2(selected_gradient.gradient.end));
				let (new_start, new_end) = (start.lerp(end, min_position), start.lerp(end, max_position));
				selected_gradient.gradient.start = transform.inverse().transform_point2(new_start);
				selected_gradient.gradient.end = transform.inverse().transform_point2(new_end);

				// Remap the positions
				for (position, _) in selected_gradient.gradient.stops.iter_mut() {
					*position = (*position - min_position) / (max_position - min_position);
				}

				// Render the new gradient
				selected_gradient.render_gradient(responses);

				self
			}
			(_, GradientToolMessage::InsertStop) => {
				for layer in document.network_interface.selected_nodes().selected_visible_layers(&document.network_interface) {
					let Some(mut gradient) = get_gradient(layer, &document.network_interface) else { continue };
					// TODO: This transform is incorrect. I think this is since it is based on the Footprint which has not been updated yet
					let transform = gradient_space_transform(layer, document);
					let mouse = input.mouse.position;
					let (start, end) = (transform.transform_point2(gradient.start), transform.transform_point2(gradient.end));

					// Compute the distance from the mouse to the gradient line in viewport space
					let distance = (end - start).angle_to(mouse - start).sin() * (mouse - start).length();

					// If click is on the line then insert point
					if distance < (SELECTION_THRESHOLD * 2.) {
						// Try and insert the new stop
						if let Some(index) = gradient.insert_stop(mouse, transform) {
							responses.add(DocumentMessage::AddTransaction);

							let mut selected_gradient = SelectedGradient::new(gradient, layer, document);

							// Select the new point
							selected_gradient.dragging = GradientDragTarget::Step(index);

							// Update the layer fill
							selected_gradient.render_gradient(responses);

							tool_data.selected_gradient = Some(selected_gradient);

							break;
						}
					}
				}

				self
			}
			(GradientToolFsmState::Ready, GradientToolMessage::PointerDown) => {
				let mouse = input.mouse.position;
				tool_data.drag_start = mouse;
				let tolerance = (MANIPULATOR_GROUP_MARKER_SIZE * 2.).powi(2);

				let mut dragging = false;
				for layer in document.network_interface.selected_nodes().selected_visible_layers(&document.network_interface) {
					let Some(gradient) = get_gradient(layer, &document.network_interface) else { continue };
					let transform = gradient_space_transform(layer, document);
					// Check for dragging step
					for (index, (pos, _)) in gradient.stops.iter().enumerate() {
						let pos = transform.transform_point2(gradient.start.lerp(gradient.end, *pos));
						if pos.distance_squared(mouse) < tolerance {
							dragging = true;
							tool_data.selected_gradient = Some(SelectedGradient {
								layer: Some(layer),
								transform,
								gradient: gradient.clone(),
								dragging: GradientDragTarget::Step(index),
							})
						}
					}

					// Check dragging start or end handle
					for (pos, dragging_target) in [(gradient.start, GradientDragTarget::Start), (gradient.end, GradientDragTarget::End)] {
						let pos = transform.transform_point2(pos);
						if pos.distance_squared(mouse) < tolerance {
							dragging = true;
							tool_data.selected_gradient = Some(SelectedGradient {
								layer: Some(layer),
								transform,
								gradient: gradient.clone(),
								dragging: dragging_target,
							})
						}
					}
				}

				let gradient_state = if dragging {
					GradientToolFsmState::Drawing
				} else {
					let selected_layer = document.click(input);

					// Apply the gradient to the selected layer
					if let Some(layer) = selected_layer {
						// Add check for raster layer
						if NodeGraphLayer::is_raster_layer(layer, &mut document.network_interface) {
							return GradientToolFsmState::Ready;
						}
						if !document.network_interface.selected_nodes().selected_layers_contains(layer, document.metadata()) {
							let nodes = vec![layer.to_node()];

							responses.add(NodeGraphMessage::SelectedNodesSet { nodes });
						}

						// Use the already existing gradient if it exists
						let gradient = if let Some(gradient) = get_gradient(layer, &document.network_interface) {
							gradient.clone()
						} else {
							// Generate a new gradient
							Gradient::new(
								DVec2::ZERO,
								global_tool_data.secondary_color,
								DVec2::ONE,
								global_tool_data.primary_color,
								DAffine2::IDENTITY,
								tool_options.gradient_type,
							)
						};
						let selected_gradient = SelectedGradient::new(gradient, layer, document).with_gradient_start(input.mouse.position);

						tool_data.selected_gradient = Some(selected_gradient);

						GradientToolFsmState::Drawing
					} else {
						GradientToolFsmState::Ready
					}
				};
				responses.add(DocumentMessage::StartTransaction);
				gradient_state
			}
			(GradientToolFsmState::Drawing, GradientToolMessage::PointerMove { constrain_axis }) => {
				if let Some(selected_gradient) = &mut tool_data.selected_gradient {
					let mouse = input.mouse.position; // tool_data.snap_manager.snap_position(responses, document, input.mouse.position);
					selected_gradient.update_gradient(mouse, responses, input.keyboard.get(constrain_axis as usize), selected_gradient.gradient.gradient_type);
				}

				// Auto-panning
				let messages = [
					GradientToolMessage::PointerOutsideViewport { constrain_axis }.into(),
					GradientToolMessage::PointerMove { constrain_axis }.into(),
				];
				tool_data.auto_panning.setup_by_mouse_position(input, &messages, responses);

				GradientToolFsmState::Drawing
			}
			(GradientToolFsmState::Drawing, GradientToolMessage::PointerOutsideViewport { .. }) => {
				// Auto-panning
				if let Some(shift) = tool_data.auto_panning.shift_viewport(input, responses) {
					if let Some(selected_gradient) = &mut tool_data.selected_gradient {
						selected_gradient.transform.translation += shift;
					}
				}

				GradientToolFsmState::Drawing
			}
			(state, GradientToolMessage::PointerOutsideViewport { constrain_axis }) => {
				// Auto-panning
				let messages = [
					GradientToolMessage::PointerOutsideViewport { constrain_axis }.into(),
					GradientToolMessage::PointerMove { constrain_axis }.into(),
				];
				tool_data.auto_panning.stop(&messages, responses);

				state
			}
			(GradientToolFsmState::Drawing, GradientToolMessage::PointerUp) => {
				input.mouse.finish_transaction(tool_data.drag_start, responses);
				tool_data.snap_manager.cleanup(responses);
				let was_dragging = tool_data.selected_gradient.is_some();

				if !was_dragging {
					if let Some(selected_layer) = document.click(input) {
						if let Some(gradient) = get_gradient(selected_layer, &document.network_interface) {
							tool_data.selected_gradient = Some(SelectedGradient::new(gradient, selected_layer, document));
						}
					}
				}
				GradientToolFsmState::Ready
			}

			(GradientToolFsmState::Drawing, GradientToolMessage::Abort) => {
				responses.add(DocumentMessage::AbortTransaction);
				tool_data.snap_manager.cleanup(responses);
				responses.add(OverlaysMessage::Draw);

				GradientToolFsmState::Ready
			}
			(_, GradientToolMessage::Abort) => GradientToolFsmState::Ready,
			_ => self,
		}
	}

	fn update_hints(&self, responses: &mut VecDeque<Message>) {
		let hint_data = match self {
			GradientToolFsmState::Ready => HintData(vec![HintGroup(vec![
				HintInfo::mouse(MouseMotion::LmbDrag, "Draw Gradient"),
				HintInfo::keys([Key::Shift], "15° Increments").prepend_plus(),
			])]),
			GradientToolFsmState::Drawing => HintData(vec![
				HintGroup(vec![HintInfo::mouse(MouseMotion::Rmb, ""), HintInfo::keys([Key::Escape], "Cancel").prepend_slash()]),
				HintGroup(vec![HintInfo::keys([Key::Shift], "15° Increments")]),
			]),
		};

		responses.add(FrontendMessage::UpdateInputHints { hint_data });
	}

	fn update_cursor(&self, responses: &mut VecDeque<Message>) {
		responses.add(FrontendMessage::UpdateMouseCursor { cursor: MouseCursorIcon::Default });
	}
}

#[cfg(test)]
mod test_gradient {
	use crate::messages::input_mapper::utility_types::input_mouse::EditorMouseState;
	use crate::messages::input_mapper::utility_types::input_mouse::ScrollDelta;
	use crate::messages::portfolio::document::{graph_operation::utility_types::TransformIn, utility_types::misc::GroupFolderType};
	pub use crate::test_utils::test_prelude::*;
	use glam::DAffine2;
	use graphene_core::vector::fill;
	use graphene_std::vector::style::Fill;

	use super::gradient_space_transform;

	async fn get_fills(editor: &mut EditorTestUtils) -> Vec<(Fill, DAffine2)> {
		let instrumented = editor.eval_graph().await;

		let document = editor.active_document();
		let layers = document.metadata().all_layers();
		layers
			.filter_map(|layer| {
				let fill = instrumented.grab_input_from_layer::<fill::FillInput<Fill>>(layer, &document.network_interface, &editor.runtime)?;
				let transform = gradient_space_transform(layer, document);
				Some((fill, transform))
			})
			.collect()
	}

	#[tokio::test]
	async fn ignore_artboard() {
		let mut editor = EditorTestUtils::create();
		editor.new_document().await;
		editor.drag_tool(ToolType::Artboard, 0., 0., 100., 100., ModifierKeys::empty()).await;
		editor.drag_tool(ToolType::Gradient, 2., 2., 4., 4., ModifierKeys::empty()).await;
		assert!(get_fills(&mut editor).await.is_empty());
	}

	#[tokio::test]
	async fn ignore_raster() {
		let mut editor = EditorTestUtils::create();
		editor.new_document().await;
		editor.create_raster_image(Image::new(100, 100, Color::WHITE), Some((0., 0.))).await;
		editor.drag_tool(ToolType::Gradient, 2., 2., 4., 4., ModifierKeys::empty()).await;
		assert!(get_fills(&mut editor).await.is_empty());
	}

	#[tokio::test]
	async fn simple_draw() {
		let mut editor = EditorTestUtils::create();
		editor.new_document().await;
		editor.drag_tool(ToolType::Rectangle, -5., -3., 100., 100., ModifierKeys::empty()).await;
		editor.select_primary_color(Color::GREEN).await;
		editor.select_secondary_color(Color::BLUE).await;
		editor.drag_tool(ToolType::Gradient, 2., 3., 24., 4., ModifierKeys::empty()).await;
		let fills = get_fills(&mut editor).await;
		assert_eq!(fills.len(), 1);
		let (fill, transform) = fills.first().unwrap();
		let gradient = fill.as_gradient().unwrap();
		// Gradient goes from secondary colour to primary colour
		let stops = gradient.stops.iter().map(|stop| (stop.0, stop.1.to_rgba8_srgb())).collect::<Vec<_>>();
		assert_eq!(stops, vec![(0., Color::BLUE.to_rgba8_srgb()), (1., Color::GREEN.to_rgba8_srgb())]);
		assert!(transform.transform_point2(gradient.start).abs_diff_eq(DVec2::new(2., 3.), 1e-10));
		assert!(transform.transform_point2(gradient.end).abs_diff_eq(DVec2::new(24., 4.), 1e-10));
	}

	#[tokio::test]
	async fn snap_simple_draw() {
		let mut editor = EditorTestUtils::create();
		editor.new_document().await;
		editor
			.handle_message(NavigationMessage::CanvasTiltSet {
				angle_radians: f64::consts::FRAC_PI_8,
			})
			.await;
		let start = DVec2::new(0., 0.);
		let end = DVec2::new(24., 4.);
		editor.drag_tool(ToolType::Rectangle, -5., -3., 100., 100., ModifierKeys::empty()).await;
		editor.drag_tool(ToolType::Gradient, start.x, start.y, end.x, end.y, ModifierKeys::SHIFT).await;
		let fills = get_fills(&mut editor).await;
		let (fill, transform) = fills.first().unwrap();
		let gradient = fill.as_gradient().unwrap();
		assert!(transform.transform_point2(gradient.start).abs_diff_eq(start, 1e-10));

		// 15 degrees from horizontal
		let angle = f64::to_radians(15.);
		let direction = DVec2::new(angle.cos(), angle.sin());
		let expected = start + direction * (end - start).length();
		assert!(transform.transform_point2(gradient.end).abs_diff_eq(expected, 1e-10));
	}

	#[tokio::test]
	async fn transformed_draw() {
		let mut editor = EditorTestUtils::create();
		editor.new_document().await;
		editor
			.handle_message(NavigationMessage::CanvasTiltSet {
				angle_radians: f64::consts::FRAC_PI_8,
			})
			.await;
		editor.drag_tool(ToolType::Rectangle, -5., -3., 100., 100., ModifierKeys::empty()).await;

		// Group rectangle
		let group_folder_type = GroupFolderType::Layer;
		editor.handle_message(DocumentMessage::GroupSelectedLayers { group_folder_type }).await;
		let metadata = editor.active_document().metadata();
		let mut layers = metadata.all_layers();
		let folder = layers.next().unwrap();
		let rectangle = layers.next().unwrap();
		assert_eq!(rectangle.parent(metadata), Some(folder));
		// Transform the group
		editor
			.handle_message(GraphOperationMessage::TransformSet {
				layer: folder,
				transform: DAffine2::from_scale_angle_translation(DVec2::new(1., 2.), 0., -DVec2::X * 10.),
				transform_in: TransformIn::Local,
				skip_rerender: false,
			})
			.await;

		editor.drag_tool(ToolType::Gradient, 2., 3., 24., 4., ModifierKeys::empty()).await;
		let fills = get_fills(&mut editor).await;
		assert_eq!(fills.len(), 1);
		let (fill, transform) = fills.first().unwrap();
		let gradient = fill.as_gradient().unwrap();
		assert!(transform.transform_point2(gradient.start).abs_diff_eq(DVec2::new(2., 3.), 1e-10));
		assert!(transform.transform_point2(gradient.end).abs_diff_eq(DVec2::new(24., 4.), 1e-10));
	}

	#[tokio::test]
	async fn double_click_insert_stop() {
		let mut editor = EditorTestUtils::create();
		editor.new_document().await;

		editor.drag_tool(ToolType::Rectangle, -5., -3., 100., 100., ModifierKeys::empty()).await;

		editor.select_primary_color(Color::GREEN).await;
		editor.select_secondary_color(Color::BLUE).await;

		editor.drag_tool(ToolType::Gradient, 0., 0., 100., 0., ModifierKeys::empty()).await;

		// Get initial gradient state (should have 2 stops)
		let initial_fills = get_fills(&mut editor).await;
		assert_eq!(initial_fills.len(), 1);
		let (initial_fill, _) = initial_fills.first().unwrap();
		let initial_gradient = initial_fill.as_gradient().unwrap();
		assert_eq!(initial_gradient.stops.len(), 2);

		editor.select_tool(ToolType::Gradient).await;

		// Simulate a double click
		let click_position = DVec2::new(50., 0.);
		let modifier_keys = ModifierKeys::empty();

		// Send the DoubleClick event
		editor
			.handle_message(InputPreprocessorMessage::DoubleClick {
				editor_mouse_state: EditorMouseState {
					editor_position: click_position,
					mouse_keys: MouseKeys::LEFT,
					scroll_delta: ScrollDelta::default(),
				},
				modifier_keys,
			})
			.await;

		// Check that a new stop has been added
		let updated_fills = get_fills(&mut editor).await;
		assert_eq!(updated_fills.len(), 1);
		let (updated_fill, _) = updated_fills.first().unwrap();
		let updated_gradient = updated_fill.as_gradient().unwrap();

		assert_eq!(updated_gradient.stops.len(), 3);

		let positions: Vec<f64> = updated_gradient.stops.iter().map(|(pos, _)| *pos).collect();
		assert!(
			positions.iter().any(|pos| (pos - 0.5).abs() < 0.1),
			"Expected to find a stop near position 0.5, but found: {:?}",
			positions
		);
	}
}
