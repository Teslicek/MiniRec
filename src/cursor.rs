use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16G16B16A16_UINT, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{Error, Result};

const VERTEX_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cursor_vs.cso"));
const PIXEL_SHADER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cursor_ps.cso"));
const XOR: u16 = 256;
const OPAQUE: u16 = 255;

#[repr(C)]
struct Placement {
    origin: [i32; 2],
    size:   [i32; 2],
    offset: [f32; 2],
    scale:  [f32; 2],
}

struct Shape {
    view:   ID3D11ShaderResourceView,
    width:  u32,
    height: u32,
}

pub struct CursorOverlay {
    vertex:     ID3D11VertexShader,
    pixel:      ID3D11PixelShader,
    rasterizer: ID3D11RasterizerState,
    placement:  ID3D11Buffer,
    desktop:    ID3D11ShaderResourceView,
    shape:      Option<Shape>,
    position:   (i32, i32),
    visible:    bool,
    buffer:     Vec<u8>,
    width:      u32,
    height:     u32,
}

impl CursorOverlay {
    pub fn new(device: &ID3D11Device, desktop: &ID3D11Texture2D, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let mut vertex = None;
            let mut pixel = None;
            let mut rasterizer = None;
            let mut placement = None;
            let mut desktop_view = None;
            let rasterizer_desc = D3D11_RASTERIZER_DESC { FillMode: D3D11_FILL_SOLID, CullMode: D3D11_CULL_NONE, DepthClipEnable: true.into(), ..Default::default() };
            let placement_desc = D3D11_BUFFER_DESC {
                ByteWidth: size_of::<Placement>() as u32,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                ..Default::default()
            };

            device.CreateVertexShader(VERTEX_SHADER, None, Some(&mut vertex))?;
            device.CreatePixelShader(PIXEL_SHADER, None, Some(&mut pixel))?;
            device.CreateRasterizerState(&rasterizer_desc, Some(&mut rasterizer))?;
            device.CreateBuffer(&placement_desc, None, Some(&mut placement))?;
            device.CreateShaderResourceView(desktop, None, Some(&mut desktop_view))?;

            Ok(Self {
                vertex: created(vertex, "vertex shader")?,
                pixel: created(pixel, "pixel shader")?,
                rasterizer: created(rasterizer, "rasterizer state")?,
                placement: created(placement, "placement buffer")?,
                desktop: created(desktop_view, "desktop view")?,
                shape: None,
                position: (0, 0),
                visible: false,
                buffer: Vec::new(),
                width,
                height,
            })
        }
    }

    pub fn update(&mut self, device: &ID3D11Device, duplication: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) -> Result<()> {
        if info.LastMouseUpdateTime == 0 {
            return Ok(());
        }

        self.visible = info.PointerPosition.Visible.as_bool();
        self.position = (info.PointerPosition.Position.x, info.PointerPosition.Position.y);

        if info.PointerShapeBufferSize == 0 {
            return Ok(());
        }

        let mut required = 0u32;
        let mut shape = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();

        self.buffer.resize(info.PointerShapeBufferSize as usize, 0);
        unsafe { duplication.GetFramePointerShape(info.PointerShapeBufferSize, self.buffer.as_mut_ptr() as *mut _, &mut required, &mut shape)? };

        let (pixels, width, height) = convert(&self.buffer, &shape)?;

        self.shape = Some(upload(device, &pixels, width, height)?);
        Ok(())
    }

    pub fn draw(&self, device: &ID3D11Device, context: &ID3D11DeviceContext, target: &ID3D11Texture2D) -> Result<()> {
        let Some(shape) = self.shape.as_ref().filter(|_| self.visible) else {
            return Ok(());
        };
        let (x, y) = self.position;
        let (width, height) = (self.width as f32, self.height as f32);
        let placement = Placement {
            origin: [x, y],
            size: [shape.width as i32, shape.height as i32],
            offset: [x as f32 / width * 2.0 - 1.0, 1.0 - y as f32 / height * 2.0],
            scale: [shape.width as f32 / width * 2.0, -(shape.height as f32) / height * 2.0],
        };
        let viewport = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: width, Height: height, MinDepth: 0.0, MaxDepth: 1.0 };
        let mut view = None;

        unsafe {
            device.CreateRenderTargetView(target, None, Some(&mut view))?;
            context.UpdateSubresource(&self.placement, 0, None, &placement as *const Placement as *const _, 0, 0);
            context.OMSetRenderTargets(Some(&[Some(created(view, "render target view")?)]), None);
            context.OMSetBlendState(None, None, u32::MAX);
            context.RSSetViewports(Some(&[viewport]));
            context.RSSetState(&self.rasterizer);
            context.IASetInputLayout(None);
            context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            context.VSSetShader(&self.vertex, None);
            context.VSSetConstantBuffers(0, Some(&[Some(self.placement.clone())]));
            context.PSSetShader(&self.pixel, None);
            context.PSSetConstantBuffers(0, Some(&[Some(self.placement.clone())]));
            context.PSSetShaderResources(0, Some(&[Some(shape.view.clone()), Some(self.desktop.clone())]));
            context.Draw(4, 0);
            context.PSSetShaderResources(0, Some(&[None, None]));
            context.OMSetRenderTargets(None, None);
        }

        Ok(())
    }
}

fn convert(buffer: &[u8], info: &DXGI_OUTDUPL_POINTER_SHAPE_INFO) -> Result<(Vec<[u16; 4]>, u32, u32)> {
    let (width, pitch) = (info.Width as usize, info.Pitch as usize);
    let shape_type = DXGI_OUTDUPL_POINTER_SHAPE_TYPE(info.Type as i32);
    let color = |height: usize, map: fn([u8; 4]) -> [u16; 4]| -> Vec<[u16; 4]> {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| y * pitch + x * 4))
            .map(|i| map([buffer[i + 2], buffer[i + 1], buffer[i], buffer[i + 3]]))
            .collect()
    };

    match shape_type {
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR => {
            let pixels = color(info.Height as usize, |[r, g, b, a]| [r as u16, g as u16, b as u16, a as u16]);

            Ok((pixels, info.Width, info.Height))
        }
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR => {
            let pixels = color(info.Height as usize, |[r, g, b, a]| match a {
                0 => [r as u16, g as u16, b as u16, OPAQUE],
                _ => [r as u16, g as u16, b as u16, XOR],
            });

            Ok((pixels, info.Width, info.Height))
        }
        DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME => {
            let height = info.Height as usize / 2;
            let bit = |row: usize, x: usize| buffer[row * pitch + x / 8] & (0x80 >> (x % 8)) != 0;
            let pixels = (0..height)
                .flat_map(|y| (0..width).map(move |x| (y, x)))
                .map(|(y, x)| match (bit(y, x), bit(y + height, x)) {
                    (false, false) => [0, 0, 0, OPAQUE],
                    (false, true) => [255, 255, 255, OPAQUE],
                    (true, false) => [0, 0, 0, 0],
                    (true, true) => [255, 255, 255, XOR],
                })
                .collect();

            Ok((pixels, info.Width, height as u32))
        }
        other => Err(Error::new(E_FAIL, format!("Unsupported cursor shape type {}", other.0))),
    }
}

fn upload(device: &ID3D11Device, pixels: &[[u16; 4]], width: u32, height: u32) -> Result<Shape> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R16G16B16A16_UINT,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_IMMUTABLE,
        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let data = D3D11_SUBRESOURCE_DATA { pSysMem: pixels.as_ptr() as *const _, SysMemPitch: width * 8, SysMemSlicePitch: 0 };
    let mut texture = None;
    let mut view = None;

    unsafe {
        device.CreateTexture2D(&desc, Some(&data), Some(&mut texture))?;
        device.CreateShaderResourceView(&created(texture, "cursor texture")?, None, Some(&mut view))?;
    }

    Ok(Shape { view: created(view, "cursor view")?, width, height })
}

fn created<T>(value: Option<T>, what: &str) -> Result<T> {
    value.ok_or_else(|| Error::new(E_FAIL, format!("Direct3D returned no {what}")))
}
