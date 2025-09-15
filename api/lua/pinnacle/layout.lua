-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, You can obtain one at https://mozilla.org/MPL/2.0/.

local client = require("pinnacle.grpc.client").client
local defs = require("pinnacle.grpc.defs")
local log = require("pinnacle.log")

---@class pinnacle.layout.LayoutArgs
---@field output pinnacle.output.OutputHandle
---@field window_count integer
---@field tags pinnacle.tag.TagHandle[]

---@alias pinnacle.layout.LayoutDir
---| "row" Lays out windows in a row horizontally.
---| "column" Lays out windows in a column vertically.

---@alias pinnacle.layout.Gaps
---A separate number of gaps per side.
---| { left: number, right: number, top: number, bottom: number }
---Gaps for all sides.
---| number

---@class pinnacle.layout.LayoutNode
---A label that helps Pinnacle decide how to diff layout trees.
---@field label string?
---An index that determines how Pinnacle traverses a layout tree.
---@field traversal_index integer?
---A set of indices per window index that changes how that window is assigned a geometry.
---@field traversal_overrides table<integer, integer[]>?
---The direction that child nodes are laid out.
---@field layout_dir pinnacle.layout.LayoutDir?
---The gaps the node applies around its children nodes.
---@field gaps (number | pinnacle.layout.Gaps)?
---The proportion the node takes up relative to its siblings.
---@field size_proportion number?
---Child layout nodes.
---@field children pinnacle.layout.LayoutNode[]?

---A layout generator.
---@class pinnacle.layout.LayoutGenerator
---Generate an array of geometries from the given `LayoutArgs`.
---@field layout fun(self: self, window_count: integer): pinnacle.layout.LayoutNode

---Builtin layout generators.
---
---This contains functions that create various builtin generators.
---@class pinnacle.layout.builtin
local builtin = {}

---A layout generator that lays out windows in a line.
---@class pinnacle.layout.builtin.Line : pinnacle.layout.LayoutGenerator
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps
---The direction that windows should be laid out in.
---@field direction pinnacle.layout.LayoutDir
---Whether or not windows are inserted backwards.
---@field reversed boolean

---Options for the line generator.
---@class pinnacle.layout.builtin.LineOpts
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps?
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps?
---The direction that windows should be laid out in.
---@field direction pinnacle.layout.LayoutDir?
---Whether or not windows are inserted backwards.
---@field reversed boolean?

---Creates a layout generator that lays out windows in a line.
---
---@param options pinnacle.layout.builtin.LineOpts? Options for the generator.
---
---@return pinnacle.layout.builtin.Line
function builtin.line(options)
    ---@type pinnacle.layout.builtin.Line
    return {
        outer_gaps = options and options.outer_gaps or 4.0,
        inner_gaps = options and options.inner_gaps or 4.0,
        direction = options and options.direction or "row",
        reversed = options and options.reversed or false,
        ---@param self pinnacle.layout.builtin.Line
        layout = function(self, window_count)
            ---@type pinnacle.layout.LayoutNode
            local root = {
                gaps = self.outer_gaps,
                layout_dir = self.direction,
                label = "builtin.line",
                children = {},
            }

            if window_count == 0 then
                return root
            end

            ---@type pinnacle.layout.LayoutNode[]
            local children = {}
            if not self.reversed then
                for i = 0, window_count - 1 do
                    table.insert(children, {
                        traversal_index = i,
                        gaps = self.inner_gaps,
                        children = {},
                    })
                end
            else
                for i = window_count - 1, 0, -1 do
                    table.insert(children, {
                        traversal_index = i,
                        gaps = self.inner_gaps,
                        children = {},
                    })
                end
            end

            root.children = children

            return root
        end,
    }
end

---A layout generator that has one master area to one side and a stack of windows next to it.
---@class pinnacle.layout.builtin.MasterStack : pinnacle.layout.LayoutGenerator
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps
---The proportion of the output the master area will take up.
---@field master_factor number
---Which side the master area will be.
---@field master_side "left" | "right" | "top" | "bottom"
---How many windows will be in the master area.
---@field master_count integer
---Reverses the direction of window insertion i.e. new windows are inserted at the top
---of the master stack instead of at the bottom of the side stack.
---@field reversed boolean

---Options for the master stack generator.
---@class pinnacle.layout.builtin.MasterStackOpts
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps?
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps?
---The proportion of the output the master area will take up.
---@field master_factor number?
---Which side the master area will be.
---@field master_side ("left" | "right" | "top" | "bottom")?
---How many windows will be in the master area.
---@field master_count integer?
---Reverses the direction of window insertion i.e. new windows are inserted at the top
---of the master stack instead of at the bottom of the side stack.
---@field reversed boolean?

---Creates a layout generator that lays windows out in two stacks: a master and side stack.
---
---@param options pinnacle.layout.builtin.MasterStackOpts? Options for the generator.
---@return pinnacle.layout.builtin.MasterStack
function builtin.master_stack(options)
    ---@type pinnacle.layout.builtin.MasterStack
    return {
        outer_gaps = options and options.outer_gaps or 4.0,
        inner_gaps = options and options.inner_gaps or 4.0,
        master_factor = options and options.master_factor or 0.5,
        master_side = options and options.master_side or "left",
        master_count = options and options.master_count or 1,
        reversed = options and options.reversed or false,
        ---@param self pinnacle.layout.builtin.MasterStack
        layout = function(self, window_count)
            ---@type pinnacle.layout.LayoutNode
            local root = {
                gaps = self.outer_gaps,
                layout_dir = (self.master_side == "left" or self.master_side == "right") and "row"
                    or "column",
                label = "builtin.master_stack",
                children = {},
            }

            if window_count == 0 then
                return root
            end

            local master_factor = math.min(math.max(0.1, self.master_factor), 0.9)

            local master_tv_idx, stack_tv_idx = 0, 1
            if self.reversed then
                master_tv_idx, stack_tv_idx = 1, 0
            end

            local master_count = math.min(self.master_count, window_count)

            local line = builtin.line({
                outer_gaps = 0.0,
                inner_gaps = self.inner_gaps,
                direction = (self.master_side == "left" or self.master_side == "right")
                        and "column"
                    or "row",
                reversed = self.reversed,
            })

            local master_side = line:layout(master_count)

            master_side.label = "builtin.master_stack.master_side"
            master_side.traversal_index = master_tv_idx
            master_side.size_proportion = master_factor * 10.0

            if window_count <= self.master_count then
                root.children = { master_side }
                return root
            end

            local stack_count = window_count - master_count
            local stack_side = line:layout(stack_count)
            stack_side.label = "builtin.master_stack.stack_side"
            stack_side.traversal_index = stack_tv_idx
            stack_side.size_proportion = (1.0 - master_factor) * 10.0

            if self.master_side == "left" or self.master_side == "top" then
                root.children = { master_side, stack_side }
            else
                root.children = { stack_side, master_side }
            end

            return root
        end,
    }
end

---A layout generator that lays out windows in a shrinking fashion towards the bottom right corner.
---@class pinnacle.layout.builtin.Dwindle : pinnacle.layout.LayoutGenerator
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps

---Options for the dwindle generator.
---@class pinnacle.layout.builtin.DwindleOpts
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps?
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps?

---Creates a layout generator that lays windows out dwindling down to the bottom right.
---
---@param options pinnacle.layout.builtin.DwindleOpts? Options for the generator.
---
---@return pinnacle.layout.builtin.Dwindle
function builtin.dwindle(options)
    ---@type pinnacle.layout.builtin.Dwindle
    return {
        outer_gaps = options and options.outer_gaps or 4.0,
        inner_gaps = options and options.inner_gaps or 4.0,
        ---@param self pinnacle.layout.builtin.Dwindle
        layout = function(self, window_count)
            ---@type pinnacle.layout.LayoutNode
            local root = {
                gaps = self.outer_gaps,
                label = "builtin.dwindle",
                children = {},
            }

            if window_count == 0 then
                return root
            end

            if window_count == 1 then
                ---@type pinnacle.layout.LayoutNode
                local child = {
                    gaps = self.inner_gaps,
                    children = {},
                }
                root.children = { child }
                return root
            end

            local current_node = root

            for i = 0, window_count - 2 do
                if current_node ~= root then
                    current_node.gaps = 0.0
                end

                ---@type pinnacle.layout.LayoutNode
                local child1 = {
                    traversal_index = 0,
                    layout_dir = (i % 2 == 0) and "column" or "row",
                    gaps = self.inner_gaps,
                    label = "builtin.dwindle.split." .. tostring(i) .. ".1",
                    children = {},
                }

                ---@type pinnacle.layout.LayoutNode
                local child2 = {
                    traversal_index = 1,
                    layout_dir = (i % 2 == 0) and "column" or "row",
                    gaps = self.inner_gaps,
                    label = "builtin.dwindle.split." .. tostring(i) .. ".2",
                    children = {},
                }

                current_node.children = { child1, child2 }

                current_node = child2
            end

            return root
        end,
    }
end

---A layout generator that lays out windows in a spiral.
---@class pinnacle.layout.builtin.Spiral : pinnacle.layout.LayoutGenerator
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps

---Options for the spiral generator.
---@class pinnacle.layout.builtin.SpiralOpts
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps?
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps?

---Creates a layout generator that lays windows out in a spiral.
---
---@param options pinnacle.layout.builtin.SpiralOpts? Options for the generator.
---
---@return pinnacle.layout.builtin.Spiral
function builtin.spiral(options)
    ---@type pinnacle.layout.builtin.Spiral
    return {
        outer_gaps = options and options.outer_gaps or 4.0,
        inner_gaps = options and options.inner_gaps or 4.0,
        ---@param self pinnacle.layout.builtin.Spiral
        layout = function(self, window_count)
            ---@type pinnacle.layout.LayoutNode
            local root = {
                gaps = self.outer_gaps,
                label = "builtin.spiral",
                children = {},
            }

            if window_count == 0 then
                return root
            end

            if window_count == 1 then
                ---@type pinnacle.layout.LayoutNode
                local child = {
                    gaps = self.inner_gaps,
                    children = {},
                }
                root.children = { child }
                return root
            end

            local current_node = root

            for i = 0, window_count - 2 do
                if current_node ~= root then
                    current_node.gaps = 0.0
                end

                ---@type pinnacle.layout.LayoutNode
                local child1 = {
                    layout_dir = (i % 2 == 0) and "column" or "row",
                    gaps = self.inner_gaps,
                    label = "builtin.spiral.split." .. tostring(i) .. ".1",
                    children = {},
                }

                ---@type pinnacle.layout.LayoutNode
                local child2 = {
                    layout_dir = (i % 2 == 0) and "column" or "row",
                    gaps = self.inner_gaps,
                    label = "builtin.spiral.split." .. tostring(i) .. ".2",
                    children = {},
                }

                current_node.children = { child1, child2 }

                if i % 4 == 0 or i % 4 == 1 then
                    child1.traversal_index = 0
                    child2.traversal_index = 1
                    current_node = child2
                else
                    child1.traversal_index = 1
                    child2.traversal_index = 0
                    current_node = child1
                end
            end

            return root
        end,
    }
end

---A layout generator that has one main corner window and a horizontal and vertical stack flanking
---it on the other two sides.
---@class pinnacle.layout.builtin.Corner : pinnacle.layout.LayoutGenerator
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps
---The proportion of the layout that the width of the corner window takes up.
---@field corner_width_factor number
---The proportion of the layout that the height of the corner window takes up.
---@field corner_height_factor number
---The location of the corner window.
---@field corner_loc "top_left" | "top_right" | "bottom_left" | "bottom_right"

---Options for the corner generator.
---@class pinnacle.layout.builtin.CornerOpts
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps?
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps?
---The proportion of the layout that the width of the corner window takes up.
---@field corner_width_factor number?
---The proportion of the layout that the height of the corner window takes up.
---@field corner_height_factor number?
---The location of the corner window.
---@field corner_loc ("top_left" | "top_right" | "bottom_left" | "bottom_right")?

---Creates a layout generator that lays windows out with one main corner window and
---a horizontal and vertical stack flanking the other two sides.
---
---@param options pinnacle.layout.builtin.CornerOpts? Options for the generator.
---
---@return pinnacle.layout.builtin.Corner
function builtin.corner(options)
    ---@type pinnacle.layout.builtin.Corner
    return {
        outer_gaps = options and options.outer_gaps or 4.0,
        inner_gaps = options and options.inner_gaps or 4.0,
        corner_width_factor = options and options.corner_width_factor or 0.5,
        corner_height_factor = options and options.corner_height_factor or 0.5,
        corner_loc = options and options.corner_loc or "top_left",
        ---@param self pinnacle.layout.builtin.Corner
        layout = function(self, window_count)
            ---@type pinnacle.layout.LayoutNode
            local root = {
                gaps = self.outer_gaps,
                label = "builtin.corner",
                children = {},
            }

            if window_count == 0 then
                return root
            end

            if window_count == 1 then
                ---@type pinnacle.layout.LayoutNode
                local child = {
                    gaps = self.inner_gaps,
                    children = {},
                }
                root.children = { child }
                return root
            end

            local corner_width_factor = math.min(math.max(0.1, self.corner_width_factor), 0.9)
            local corner_height_factor = math.min(math.max(0.1, self.corner_height_factor), 0.9)

            ---@type pinnacle.layout.LayoutNode
            local corner_and_horiz_stack_node = {
                traversal_index = 0,
                label = "builtin.corner.corner_and_stack",
                layout_dir = "column",
                size_proportion = corner_width_factor * 10.0,
                children = {},
            }

            local vert_count = math.ceil((window_count - 1) / 2)
            local horiz_count = math.floor((window_count - 1) / 2)

            local vert_stack = builtin.line({
                outer_gaps = 0.0,
                inner_gaps = self.inner_gaps,
                direction = "column",
                reversed = false,
            })

            local vert_stack_node = vert_stack:layout(vert_count)
            vert_stack_node.size_proportion = (1.0 - corner_width_factor) * 10.0
            vert_stack_node.traversal_index = 1

            if self.corner_loc == "top_left" or self.corner_loc == "bottom_left" then
                root.children = { corner_and_horiz_stack_node, vert_stack_node }
            else
                root.children = { vert_stack_node, corner_and_horiz_stack_node }
            end

            if horiz_count == 0 then
                corner_and_horiz_stack_node.gaps = self.inner_gaps
                return root
            end

            ---@type pinnacle.layout.LayoutNode
            local corner_node = {
                traversal_index = 0,
                size_proportion = corner_height_factor * 10.0,
                gaps = self.inner_gaps,
                children = {},
            }

            local horiz_stack = builtin.line({
                outer_gaps = 0.0,
                inner_gaps = self.inner_gaps,
                direction = "row",
                reversed = false,
            })

            local horiz_stack_node = horiz_stack:layout(horiz_count)
            horiz_stack_node.size_proportion = (1.0 - corner_height_factor) * 10.0
            horiz_stack_node.traversal_index = 1

            if self.corner_loc == "top_left" or self.corner_loc == "top_right" then
                corner_and_horiz_stack_node.children = { corner_node, horiz_stack_node }
            else
                corner_and_horiz_stack_node.children = { horiz_stack_node, corner_node }
            end

            local traversal_overrides = {}
            for i = 0, window_count - 1 do
                traversal_overrides[i] = { i % 2 }
            end

            root.traversal_overrides = traversal_overrides

            return root
        end,
    }
end

---A layout generator that attempts to lay out windows such that they are the same size.
---@class pinnacle.layout.builtin.Fair : pinnacle.layout.LayoutGenerator
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps
---Which axis the lines of windows will run.
---@field axis "horizontal" | "vertical"

---Options for the fair generator.
---@class pinnacle.layout.builtin.FairOpts
---The gaps between the outer container and this layout.
---@field outer_gaps pinnacle.layout.Gaps?
---The gaps between windows within this layout.
---@field inner_gaps pinnacle.layout.Gaps?
---Which axis the lines of windows will run.
---@field axis ("horizontal" | "vertical")?

---Creates a layout generator that lays windows out keeping their sizes roughly the same.
---
---@param options pinnacle.layout.builtin.FairOpts? Options for the generator.
---
---@return pinnacle.layout.builtin.Fair
function builtin.fair(options)
    ---@type pinnacle.layout.builtin.Fair
    return {
        outer_gaps = options and options.outer_gaps or 4.0,
        inner_gaps = options and options.inner_gaps or 4.0,
        axis = options and options.axis or "vertical",
        ---@param self pinnacle.layout.builtin.Fair
        layout = function(self, window_count)
            ---@type pinnacle.layout.LayoutNode
            local root = {
                gaps = self.outer_gaps,
                label = "builtin.fair",
                children = {},
            }

            if window_count == 0 then
                return root
            end

            if window_count == 1 then
                ---@type pinnacle.layout.LayoutNode
                local child = {
                    gaps = self.inner_gaps,
                    label = "builtin.fair.line.1",
                    children = {},
                }
                root.children = { child }
                return root
            end

            if window_count == 2 then
                ---@type pinnacle.layout.LayoutNode
                local child1 = {
                    gaps = self.inner_gaps,
                    label = "builtin.fair.line.1",
                    children = {},
                }
                ---@type pinnacle.layout.LayoutNode
                local child2 = {
                    gaps = self.inner_gaps,
                    label = "builtin.fair.line.2",
                    children = {},
                }
                root.children = { child1, child2 }
                return root
            end

            local line_count = math.floor(math.sqrt(window_count) + 0.5)
            local wins_per_line = {}

            local max_per_line = (window_count > line_count * line_count) and line_count + 1
                or line_count

            for i = 1, window_count do
                local index = math.ceil(i / max_per_line)
                if not wins_per_line[index] then
                    wins_per_line[index] = 0
                end
                wins_per_line[index] = wins_per_line[index] + 1
            end

            local line = builtin.line({
                outer_gaps = 0.0,
                inner_gaps = self.inner_gaps,
                direction = self.axis == "horizontal" and "row" or "column",
                reversed = false,
            })

            ---@type pinnacle.layout.LayoutNode[]
            local lines = {}
            for i = 1, line_count do
                lines[i] = line:layout(wins_per_line[i])
                lines[i].label = "builtin.fair.line." .. tostring(i)
            end

            root.children = lines

            root.layout_dir = self.axis == "horizontal" and "column" or "row"

            return root
        end,
    }
end

---A layout generator that floats windows.
---
---This works by simply returning an empty layout tree.
---Note: the windows are not truly floating, see `WindowHandle::spilled` for
---details.
---@class pinnacle.layout.builtin.Floating : pinnacle.layout.LayoutGenerator

---Creates a layout generator that floats windows.
---
---@return pinnacle.layout.builtin.Floating
function builtin.floating()
    return {
        layout = function(self, window_count)
            return {
                label = "builtin.floating",
                children = {},
            }
        end,
    }
end

---A layout generator that keeps track of layouts per tag
---and provides methods to cycle between them.
---@class pinnacle.layout.builtin.Cycle : pinnacle.layout.LayoutGenerator
---The layouts this generator will cycle between.
---@field layouts pinnacle.layout.LayoutGenerator[]
---@field private tag_indices table<integer, integer>
---The current tag that will determine the chosen layout.
---@field current_tag pinnacle.tag.TagHandle?
local Cycle = {}

---Cycles the layout forward for the given tag.
---
---@param tag pinnacle.tag.TagHandle The tag to cycle the layout for.
function Cycle:cycle_layout_forward(tag)
    if not self.tag_indices[tag.id] then
        self.tag_indices[tag.id] = 1
    end
    self.tag_indices[tag.id] = self.tag_indices[tag.id] + 1
    if self.tag_indices[tag.id] > #self.layouts then
        self.tag_indices[tag.id] = 1
    end
end

---Cycles the layout backward for the given tag.
---
---@param tag pinnacle.tag.TagHandle The tag to cycle the layout for.
function Cycle:cycle_layout_backward(tag)
    if not self.tag_indices[tag.id] then
        self.tag_indices[tag.id] = 1
    end
    self.tag_indices[tag.id] = self.tag_indices[tag.id] - 1
    if self.tag_indices[tag.id] < 1 then
        self.tag_indices[tag.id] = #self.layouts
    end
end

---Gets the current layout generator for the given tag.
---
---@param tag pinnacle.tag.TagHandle The tag to get a layout for.
---
---@return pinnacle.layout.LayoutGenerator?
function Cycle:current_layout(tag)
    return self.layouts[self.tag_indices[tag.id] or 1]
end

---Gets a (most-likely) unique identifier for the current layout tree.
---This is guaranteed to be greater than zero.
---
---@return integer
function Cycle:current_tree_id()
    local tag_id = self.current_tag and self.current_tag.id or 0
    local layout_id = self.tag_indices[tag_id] or 1
    -- Can't use bit fiddling as bit32 doesn't exist on 5.4 and 5.2 doesn't have the syntax for
    -- bitwise operators, so this is close enough
    return tag_id + layout_id * 9999999 + 1
end

---Creates a layout generator that delegates to other layout generators depending on the tag
---and allows you to cycle between the generators.
---
---@param layouts pinnacle.layout.LayoutGenerator[] The layouts that this generator will cycle between.
---
---@return pinnacle.layout.builtin.Cycle
function builtin.cycle(layouts)
    ---@type pinnacle.layout.builtin.Cycle
    local cycler = {
        layouts = layouts,
        tag_indices = {},
        current_tag = nil,
        ---@param self pinnacle.layout.builtin.Cycle
        layout = function(self, window_count)
            if self.current_tag then
                local curr_layout = self:current_layout(self.current_tag)
                if curr_layout then
                    return curr_layout:layout(window_count)
                end
            end

            ---@type pinnacle.layout.LayoutNode
            local node = {
                children = {},
            }

            return node
        end,
    }

    setmetatable(cycler, { __index = Cycle })
    return cycler
end

---Layout management.
---
---Read the [wiki page](https://pinnacle-comp.github.io/pinnacle/configuration/layout.html) for more information.
---
---@class pinnacle.layout
local layout = {
    ---Builtin layout generators.
    builtin = builtin,
}

---An object that allows you to forcibly request layouts.
---@class pinnacle.layout.LayoutRequester
---@field private sender grpc_client.h2.Stream
local LayoutRequester = {}

---Causes the compositor to emit a layout request.
---
---@param output pinnacle.output.OutputHandle? The output to layout, or `nil` for the focused output.
function LayoutRequester:request_layout(output)
    local output = output or require("pinnacle.output").get_focused()
    if not output then
        return
    end

    local chunk = require("pinnacle.grpc.protobuf").encode("pinnacle.layout.v1.LayoutRequest", {
        force_layout = {
            output_name = output.name,
        },
    })

    local success, err = pcall(self.sender.write_chunk, self.sender, chunk)

    if not success then
        print("error sending to stream:", err)
    end
end

---@param node pinnacle.layout.LayoutNode
---
---@return pinnacle.layout.v1.LayoutNode
local function layout_node_to_api_node(node)
    local traversal_overrides = {}
    for idx, overrides in pairs(node.traversal_overrides or {}) do
        traversal_overrides[idx] = {
            overrides = overrides,
        }
    end

    local gaps = node.gaps or 0.0
    if type(gaps) == "number" then
        local gaps_num = gaps
        gaps = {
            left = gaps_num,
            right = gaps_num,
            top = gaps_num,
            bottom = gaps_num,
        }
    end

    local children = {}
    for _, child in ipairs(node.children or {}) do
        table.insert(children, layout_node_to_api_node(child))
    end

    ---@type pinnacle.layout.v1.LayoutNode
    return {
        label = node.label,
        traversal_overrides = traversal_overrides,
        traversal_index = node.traversal_index or 0,
        style = {
            size_proportion = node.size_proportion or 1.0,
            flex_dir = ((node.layout_dir or "row") == "row")
                    and defs.pinnacle.layout.v1.FlexDir.FLEX_DIR_ROW
                or defs.pinnacle.layout.v1.FlexDir.FLEX_DIR_COLUMN,
            gaps = gaps,
        },
        children = children,
    }
end

---A response to a layout request.
---@class pinnacle.layout.LayoutResponse
---The root node of the layout tree.
---@field root_node pinnacle.layout.LayoutNode
---A non-negative identifier.
---
---Trees that are considered "the same", like trees for a certain tag and layout,
---should have the same identifier to allow Pinnacle to remember tile sizing.
---@field tree_id integer

---Begins managing layout requests from the compositor.
---
---You must call this function to get windows to tile.
---The provided function will be run with the arguments of the layout request.
---It must return a `LayoutResponse` containing a `LayoutNode` that represents
---the root of a layout tree, along with an identifier.
---
---#### Example
---
---```lua
---local layout_requester = Layout.manage(function(args)
---    local first_tag = args.tags[1]
---    if not first_tag then
---        ---@type pinnacle.layout.LayoutResponse
---        return {
---            root_node = {},
---            tree_id = 0,
---        }
---    end
---    layout_cycler.current_tag = first_tag
---    local root_node = layout_cycler:layout(args.window_count)
---    local tree_id = layout_cycler:current_tree_id()
---
---    ---@type pinnacle.layout.LayoutResponse
---    return {
---        root_node = root_node,
---        tree_id = tree_id,
---    }
---end)
---```
---
---@param on_layout fun(args: pinnacle.layout.LayoutArgs): pinnacle.layout.LayoutResponse A function that receives layout arguments and builds and returns a layout response.
---
---@return pinnacle.layout.LayoutRequester # A requester that allows you to force the compositor to request a layout.
---@nodiscard
function layout.manage(on_layout)
    local stream, err = client:pinnacle_layout_v1_LayoutService_Layout(function(response, stream)
        ---@type pinnacle.layout.LayoutArgs
        local args = {
            output = require("pinnacle.output").handle.new(response.output_name),
            window_count = response.window_count,
            tags = require("pinnacle.tag").handle.new_from_table(response.tag_ids or {}),
        }

        local success, ret = pcall(on_layout, args)
        if not success then
            log.error("In Layout:manage: " .. tostring(ret))
            ret = {
                root_node = {},
                tree_id = 0,
            }
        end

        ---@type pinnacle.layout.LayoutNode
        local node

        -- FORWARD-COMPAT: v0.1.0 allowed returning just a `pinnacle.layout.LayoutNode`.
        -- Remove in v0.4.0
        if not ret.root_node then
            node = ret --[[@as pinnacle.layout.LayoutNode]]
        else
            node = ret.root_node
        end

        local tree_id = ret.tree_id or 0

        local chunk = require("pinnacle.grpc.protobuf").encode("pinnacle.layout.v1.LayoutRequest", {
            tree_response = {
                request_id = response.request_id,
                tree_id = tree_id,
                output_name = response.output_name,
                root_node = layout_node_to_api_node(node),
            },
        })

        local success, err = pcall(stream.write_chunk, stream, chunk)

        if not success then
            print("error sending to stream:", err)
        end
    end)

    if err then
        log.error("failed to start bidir stream")
        os.exit(1)
    end

    local requester = { sender = stream }
    setmetatable(requester, { __index = LayoutRequester })

    return requester
end

return layout
