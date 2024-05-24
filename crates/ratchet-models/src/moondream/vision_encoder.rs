use std::{
    error::Error,
    io::{BufRead, Seek},
};

use hf_hub::api::sync::Api;
use ratchet::{prelude::shape, Device, DeviceRequest, Tensor};
use ratchet_loader::gguf::{self, gguf::Header};
use ratchet_nn::{LayerNorm, Linear, Module};

#[derive(Debug, derive_new::new)]
struct Attention {
    n_heads: usize,
    dim: usize,
    qkv: Linear,
    proj: Linear,
    scale_factor: Tensor,
}

impl Module for Attention {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        let h_dim = self.dim / self.n_heads;
        let [b, n, c]: [usize; 3] = input.shape().try_into()?;
        // step 1 - 0, 1, 2, 3, 4
        // step 2 - 0, 2, 1, 3, 4
        // step 3 - 2, 0, 1, 3, 4
        // step 4 - 2, 0, 3, 1, 4

        // b, n, 3, nh, hd
        let mut qkv = self.qkv.schedule(input.clone())?;
        // b, 3, n, nh, hd
        qkv = qkv
            .view(shape![b, n, 3, self.n_heads * h_dim])?
            .permute(&[0, 2, 1, 3])?;
        // 3, b, n, nh, hd
        qkv = qkv
            .view(shape![b, 3, n * self.n_heads * h_dim])?
            .permute(&[1, 0, 2])?;
        // 3, b, nh, n, hd
        qkv = qkv
            .view(shape![3 * b, n, self.n_heads, h_dim])?
            .permute(&[0, 2, 1, 3])?
            .view(shape![3, b * self.n_heads * n * h_dim])?;

        let q = qkv
            .clone()
            .slice(&[0..1, 0..b * self.n_heads * n * h_dim])?
            .view(shape![b, self.n_heads, n, h_dim])?;
        let k = qkv
            .clone()
            .slice(&[1..2, 0..b * self.n_heads * n * h_dim])?
            .view(shape![b, self.n_heads, n, h_dim])?;
        let v = qkv
            .clone()
            .slice(&[2..3, 0..b * self.n_heads * n * h_dim])?
            .view(shape![b, self.n_heads, n, h_dim])?;

        // scaled dot-product attention
        let mut attn_weights = q
            .matmul(k.permute(&[0, 1, 3, 2])?, false, false)?
            .mul(self.scale_factor.clone())?;
        attn_weights = attn_weights.softmax(3)?;
        let mut x = attn_weights.matmul(v, false, false)?;
        x = x.permute(&[0, 1, 3, 2])?.view(shape![b, n, c])?;
        self.proj.schedule(x)
    }
}

#[derive(Debug, derive_new::new)]
struct MLP {
    fc1: Linear,
    fc2: Linear,
}

impl Module for MLP {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        self.fc2.schedule(self.fc1.schedule(input)?.gelu()?)
    }
}

#[derive(Debug, derive_new::new)]
struct VitBlock {
    embed_dim: usize,
    attn: Attention,
    mlp: MLP,
    norm1: LayerNorm,
    norm2: LayerNorm,
}

impl Module for VitBlock {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        let x = input
            .clone()
            .add(self.attn.schedule(self.norm1.schedule(input)?)?)?;
        x.clone().add(self.mlp.schedule(self.norm2.schedule(x)?)?)
    }
}

#[derive(Debug, derive_new::new)]
struct LinearPatchEmbedding {
    linear: Linear,
}

impl Module for LinearPatchEmbedding {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        let [b, c, hp1, wp2]: [usize; 4] = input.shape().try_into()?;
        let (p1, p2) = (14_usize, 14_usize);
        let (h, w) = (hp1 / p1, wp2 / p2);
        // step 1 - 0, 1, 2, 3, 4, 5
        // step 2 - 0, 2, 1, 3, 4, 5
        // step 3 - 0, 2, 1, 4, 3, 5
        // step 4 - 0, 2, 4, 1, 3, 5

        // b, c, h, p1, w, p2
        let mut x = input
            .view(shape![b, c, h, p1 * w * p2])?
            .permute(&[0, 2, 1, 3])?;
        // b, h, c, p1, w, p2
        x = x
            .view(shape![b * h * c, p1, w, p2])?
            .permute(&[0, 2, 1, 3])?;
        // b, h, c, w, p1, p2
        x = x
            .view(shape![b * h, c, p1, w * p2])?
            .permute(&[0, 2, 1, 3])?;
        // b, h, w, c, p1, p2
        x = x.view(shape![b, h * w, c * p1 * p2])?;
        self.linear.schedule(x)
    }
}

#[derive(Debug, derive_new::new)]
struct VisionTransformer {
    patch_embed: LinearPatchEmbedding,
    pos_embed: Tensor,
    blocks: Vec<VitBlock>,
    norm: LayerNorm,
}

impl Module for VisionTransformer {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        println!("{:?}", input.shape());
        let mut x = self.patch_embed.schedule(input)?;
        x = x.clone().add(self.pos_embed.clone())?;
        x = self
            .blocks
            .iter()
            .fold(x.clone(), |acc, blk| blk.schedule(acc).unwrap());
        self.norm.schedule(x)
    }
}

struct VisionProjection {
    mlp: MLP,
}

impl Module for VisionProjection {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        self.mlp.schedule(input)
    }
}

struct VisionEncoder {
    projection: VisionProjection,
    transformer: VisionTransformer,
}

impl Module for VisionEncoder {
    type Input = Tensor;

    fn schedule(&self, input: Self::Input) -> anyhow::Result<Tensor> {
        self.projection.schedule(self.transformer.schedule(input)?)
    }
}

impl VisionEncoder {
    pub fn load<R: BufRead + Seek>(
        disk_model: &Header,
        reader: &mut R,
        device: &Device,
    ) -> anyhow::Result<Self> {
        let lt = |name: &str| disk_model.tensor(reader, &name, device);
        Self::load_inner(disk_model, lt, device)
    }

    fn load_inner<F>(header: &Header, mut lt: F, device: &Device) -> anyhow::Result<Self>
    where
        F: FnMut(&str) -> anyhow::Result<Tensor>,
    {
        let projection = VisionProjection {
            mlp: MLP {
                fc1: Linear::new(lt("mm.0.weight")?, Some(lt("mm.0.bias")?)),
                fc2: Linear::new(lt("mm.2.weight")?, Some(lt("mm.2.bias")?)),
            },
        };
        let attn_ln_eps = header
            .metadata
            .get("clip.vision.attention.layer_norm_epsilon")
            .unwrap()
            .to_f32()
            .unwrap();
        let transformer = VisionTransformer {
            patch_embed: LinearPatchEmbedding {
                linear: Linear::new(
                    lt("v.patch_embd.weight")?.view(shape![1152, 588])?,
                    Some(lt("v.patch_embd.bias")?),
                ),
            },
            pos_embed: lt("v.position_embd.weight")?,
            blocks: (0..28)
                .map(|layer| {
                    let qw = lt(&format!("v.blk.{}.attn_q.weight", layer)).unwrap();
                    let kw = lt(&format!("v.blk.{}.attn_k.weight", layer)).unwrap();
                    let vw = lt(&format!("v.blk.{}.attn_v.weight", layer)).unwrap();
                    let qb = lt(&format!("v.blk.{}.attn_q.bias", layer)).unwrap();
                    let kb = lt(&format!("v.blk.{}.attn_k.bias", layer)).unwrap();
                    let vb = lt(&format!("v.blk.{}.attn_v.bias", layer)).unwrap();
                    let qkvw = Tensor::cat(vec![qw, kw, vw].into(), 0).unwrap();
                    let qkvb = Tensor::cat(vec![qb, kb, vb].into(), 0).unwrap();

                    let n_heads = header
                        .metadata
                        .get("clip.vision.attention.head_count")
                        .unwrap()
                        .to_u32()
                        .unwrap()
                        .try_into()
                        .unwrap();
                    let dim = 1152;
                    let h_dim = dim / n_heads;
                    let scale_factor =
                        Tensor::from_data([1.0 / (h_dim as f32).sqrt()], shape![1], device.clone());

                    VitBlock {
                        embed_dim: header
                            .metadata
                            .get("clip.vision.embedding_length")
                            .unwrap()
                            .to_u32()
                            .unwrap()
                            .try_into()
                            .unwrap(),
                        attn: Attention {
                            n_heads: n_heads,
                            dim: 1152,
                            qkv: Linear::new(qkvw, Some(qkvb)),
                            proj: Linear::new(
                                lt(&format!("v.blk.{}.attn_out.weight", layer)).unwrap(),
                                Some(lt(&format!("v.blk.{}.attn_out.bias", layer)).unwrap()),
                            ),
                            scale_factor: scale_factor,
                        },
                        mlp: MLP {
                            fc1: Linear::new(
                                lt(&format!("v.blk.{}.ffn_down.weight", layer)).unwrap(),
                                Some(lt(&format!("v.blk.{}.ffn_down.bias", layer)).unwrap()),
                            ),
                            fc2: Linear::new(
                                lt(&format!("v.blk.{}.ffn_up.weight", layer)).unwrap(),
                                Some(lt(&format!("v.blk.{}.ffn_up.bias", layer)).unwrap()),
                            ),
                        },
                        norm1: LayerNorm::new(
                            lt(&format!("v.blk.{}.ln1.weight", layer)).unwrap(),
                            Some(lt(&format!("v.blk.{}.ln1.bias", layer)).unwrap()),
                            attn_ln_eps,
                        ),
                        norm2: LayerNorm::new(
                            lt(&format!("v.blk.{}.ln2.weight", layer)).unwrap(),
                            Some(lt(&format!("v.blk.{}.ln2.bias", layer)).unwrap()),
                            attn_ln_eps,
                        ),
                    }
                })
                .collect::<Vec<_>>(),
            norm: LayerNorm::new(
                lt("v.post_ln.weight")?,
                Some(lt("v.post_ln.bias")?),
                attn_ln_eps,
            ),
        };
        Ok(VisionEncoder {
            projection: projection,
            transformer: transformer,
        })
    }
}

#[test]
fn load() {
    let api = Api::new().unwrap();
    let model = api.model("vikhyatk/moondream2".to_string());
    let model_path = model.get("moondream2-mmproj-f16.gguf").unwrap();
    let mut reader = std::io::BufReader::new(std::fs::File::open(model_path).unwrap());
    let device = Device::request_device(DeviceRequest::GPU).unwrap();
    let content = gguf::gguf::Header::read(&mut reader).unwrap();
    let model = VisionEncoder::load(&content, &mut reader, &device).unwrap();
    let input = Tensor::randn::<f32>(shape![1, 3, 378, 378], device);
    let out = model.schedule(input).unwrap();
    println!("{:?}", out.resolve().unwrap().to(&Device::CPU).unwrap());
}