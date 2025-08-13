# CrowdCent Style Guide

## Design Philosophy

Our design language draws inspiration from brutalist and minimalist UI principles, combining stark geometric shapes, functional interfaces, and a high-contrast color palette. The aesthetic emphasizes:

- **Utilitarian Structure**: Raw, unadorned interfaces with clear purpose
- **Technological Tension**: Sharp angles and patterns that create visual urgency
- **Data Visualization**: Interface elements that communicate system status
- **Functional Typography**: Clear, readable text prioritized over decorative elements
- **Angular Geometry**: Geometric shapes and intersecting lines creating structured composition

## Color Palette

### Primary Colors
- **Cyan Accent** `#62e4fb` - Critical interface elements, highlights, data visualization
- **Deep Purple** `#4152A8` - Secondary elements, supporting visuals, depth indicators
- **Dark Abyss** `#001926` - Primary background color, creating high contrast with UI elements
- **Secondary Blue** `#005380` - Supporting elements and gradient components

### UI State Colors
- **Warning** - Pattern of alternating `#001926` and `#4152A8` stripes
- **Active State** - Glowing cyan elements with increased opacity
- **Dormant State** - Reduced opacity, desaturated elements
- **Alert** - High-contrast, pulsing elements using primary accent colors

## Typography

### Font Hierarchy
- **Monospace Display Text** - For critical data, status indicators
- **Sans-serif Interface Text** - For general UI elements and content
- **Condensed Text** - For space-efficient data presentation

### Text Treatments
- `.text-accent` - Cyan accented text for critical information
- `.text-secondary` - Deep purple text for supporting information 
- `.text-muted` - Reduced emphasis text
- `blockquote` - Special treatment for highlighted content with distinctive styling

## UI Components

### Interface Containers
- **Angular Cards** - `.card` - Sharp-edged content containers with minimal styling
- **Brutalist Frames** - Stark, utilitarian content boundaries with exposed structure

### Control Elements
- **Primary Buttons** - `.btn-primary` - High-contrast action elements
- **System Toggles** - `.form-check-input` - Binary state controls with visual feedback
- **Data Input Fields** - `.form-control` - Clearly demarcated input areas
- **Progress Indicators** - Static bars showing system status

### Navigation
- **Primary Navigation** - `.navbar-dark.bg-primary` - Top-level navigation system
- **Brutalist Dropdowns** - Stark, angular dropdown menus
- **Section Divisions** - Clear visual separation between functional areas

## Visual Patterns

### Geometric Structures
- **Intersecting Lines** - Creating visual tension and direction
- **Angular Brackets** - `[`, `]`, `<`, `>` shapes for UI containment
- **Cross Patterns** - Structural intersections for visual order

### Interface Elements
- **Scanning Lines** - Animated elements suggesting system monitoring
- **Data Flow Paths** - Connecting lines showing information transfer
- **Angular Corners** - Sharp, precise edges on containers and divisions
- **Warning Patterns** - Diagonal striping for cautionary indicators

### Status Visualization
- **Glowing Nodes** - Indicating active system components
- **Pulse Animations** - Subtle rhythmic animations showing system activity
- **Scanning Effects** - Directional movement indicating monitoring
- **Progress Bars** - Vertical and horizontal indicators of system status

## Illustrations

Our illustrations follow the same brutalist principles, featuring:

- **Grid Systems** - Underlying structure visible in the background
- **Geometric Networks** - Connected nodes and pathways
- **Technical Crosshairs** - Targeting and focus elements
- **Angular Construction** - Sharp corners and precise geometry
- **Data Visualization** - Abstract representations of system activity

## Responsive Behavior

- **Structural Integrity** - Maintain angular geometry at all breakpoints
- **Reduced Animation** - Simplified motion on smaller devices
- **Stacked Hierarchy** - Vertical reorganization of elements on mobile
- **Preserved Contrast** - Maintain readability with high contrast at all sizes

## Practical Application

### Interface Example: Data Dashboard
- Sharp-angled cards for data display
- Cyan accent for critical metrics
- Deep purple for secondary metrics
- Scanning animation for active monitoring
- Angular brackets for section divisions

### Interface Example: Form Controls
- High-contrast input fields
- Geometric button shapes
- Visible grid alignment
- Technical, precise labeling
- Stark visual feedback on interaction

## Error Pages

Our error pages follow a unified design approach to maintain consistency across different HTTP error codes while providing distinct visual cues for each type of error:

### Error Page Structure
- **Unified Template** - All error pages use a single `error.html` template with conditional content
- **Two-Column Layout** - Text information on the left, visual representation on the right
- **Consistent Navigation** - All error pages provide clear paths back to safe areas

### Error-Specific Visual Language
- **404 Not Found** - Cyan angular elements with scanning elements
- **403 Forbidden** - Red lock symbol with forbidden indicator
- **429 Rate Limited** - Yellow warning triangle with clock
- **500 Server Error** - Red server rack with electrical failure indicators
- **Generic Errors** - Purple circular pattern for other HTTP codes

### Error Page Components
- Error code prominently displayed
- Brief, clear explanation of the error
- Visual representation matching the error type
- Actionable suggestions for resolution
- Links to navigate away from the error state
- System analysis details for those seeking more information

---

This style guide establishes a cohesive system that balances the raw, exposed structure of brutalist and minimalist design with the technical precision of advanced interface systems. The visual language communicates functionality, system status, and hierarchical organization while maintaining a distinctive aesthetic identity. Edward Tufte's principles should always apply.